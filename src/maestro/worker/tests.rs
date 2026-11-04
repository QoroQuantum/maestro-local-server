use super::*;
use crate::maestro::runner::TaskType;
use std::io::Write;
use std::sync::{Arc, mpsc};
use std::time::Instant;

const TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) fn task(qasm: &str) -> Task {
    Task::new(
        1,
        1,
        TaskType::Execute,
        qasm.to_owned(),
        String::new(),
        1,
        1,
        1,
        r#"{"shots":1}"#.to_owned(),
    )
}

pub(crate) fn wait_for_worker(execution: &Execution) -> u32 {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        if let Some(child) = &execution.process.lock().unwrap().child {
            return child.id();
        }
        assert!(Instant::now() < deadline, "Worker did not start");
        thread::sleep(Duration::from_millis(1));
    }
}

fn command(mode: &str) -> Command {
    let mut command = worker_command().unwrap();
    command.env("MAESTRO_TEST_WORKER_MODE", mode);
    command
}

// This entry point exists only in the unit-test executable. Most requests run
// the real native code; special requests let us reproduce hangs/crashes without
// relying on the speed or memory usage of a particular simulation backend.
#[test]
fn worker_entry() {
    if std::env::var_os("MAESTRO_TEST_WORKER").is_none() {
        return;
    }
    if std::env::var_os("MAESTRO_TEST_PRODUCTION_WORKER").is_some() {
        match run() {
            Ok(()) => std::process::exit(0),
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(error.exit_code());
            }
        }
    }
    let result = serve_test_worker();
    if let Err(error) = result {
        eprintln!("Test worker failed: {error}");
        std::process::exit(1);
    }
    std::process::exit(0);
}

fn hang() -> ! {
    loop {
        thread::park();
    }
}

fn serve_test_worker() -> io::Result<()> {
    let mut mode = std::env::var("MAESTRO_TEST_WORKER_MODE").unwrap_or_default();

    if mode == "exit" {
        std::process::exit(42);
    }
    let mut socket = worker_socket()?;
    protocol::write_ready(&mut socket)?;
    if mode == "no_read" {
        hang();
    }
    let mut task = protocol::read_task(&mut socket)?;
    if let Some(requested) = task.qasm.strip_prefix("test:") {
        mode = requested.to_owned();
    }
    if let Some(requested) = json::parse(&task.qasm)
        .ok()
        .and_then(|value| value["test_worker"].as_str().map(str::to_owned))
    {
        mode = requested;
    }
    if mode == "logs_on_cancel" || mode == "logs_on_cancel_failure" {
        let cancelled = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(libc::SIGTERM, Arc::clone(&cancelled))?;
        eprintln!("Worker ready for cancellation");
        while !cancelled.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(1));
        }
        eprintln!(
            "{}\nFinal output during cancellation",
            "x".repeat(128 * 1024)
        );
        if mode == "logs_on_cancel_failure" {
            return Err(io::Error::other("Cleanup could not be confirmed"));
        }
        return Ok(());
    }
    if mode == "hang" {
        hang();
    }
    if mode == "truncated" {
        socket.write_all(&1000_u64.to_be_bytes())?;
        socket.write_all(b"{")?;
        return Ok(());
    }
    if mode == "wrong_task" || mode == "wrong_task_cleanup_failure" {
        task.task_id += 1;
    }
    let result = match mode.as_str() {
        "malformed" => "not JSON".to_owned(),
        "flood" => {
            eprint!(
                "Startup diagnostic\n{}\nLate failure diagnostic\n",
                "x".repeat(256 * 1024)
            );
            r#"{"counts":{"0":1}}"#.into()
        }
        "large" => json::object! {data: "x".repeat(2 * 1024 * 1024)}.dump(),
        "response_then_hang"
        | "exit_after_result"
        | "wrong_task"
        | "wrong_task_cleanup_failure"
        | "logs" => {
            println!("Native diagnostic on stdout");
            eprintln!("Native diagnostic on stderr");
            r#"{"counts":{"0":1}}"#.to_owned()
        }
        _ => task.execute(),
    };
    protocol::write_result(&mut socket, &task, &result, None)?;
    if mode == "wrong_task_cleanup_failure" {
        std::process::exit(75);
    }
    if mode == "response_then_hang" {
        hang();
    }
    if mode == "exit_after_result" {
        std::process::exit(42);
    }
    Ok(())
}

#[test]
fn task_protocol_preserves_all_fields() {
    let original = Task::new(
        u32::MAX,
        12,
        TaskType::Estimate,
        "OPENQASM 2.0;\ninclude \"qelib1.inc\";\r\n".to_owned(),
        "Z;\nX".to_owned(),
        7,
        2,
        3,
        "{\n\"shots\": 100\n}".to_owned(),
    );
    let mut data = Vec::new();
    protocol::write_task(&mut data, &original).unwrap();
    let decoded = protocol::read_task(&mut data.as_slice()).unwrap();
    assert_eq!(decoded.session_id, original.session_id);
    assert_eq!(decoded.task_id, original.task_id);
    assert!(matches!(decoded.task_type, TaskType::Estimate));
    assert_eq!(decoded.qasm, original.qasm);
    assert_eq!(decoded.observables, original.observables);
    assert_eq!(decoded.nr_qubits, original.nr_qubits);
    assert_eq!(decoded.sim_type, original.sim_type);
    assert_eq!(decoded.method, original.method);
    assert_eq!(decoded.options, original.options);
}

#[test]
fn cancellation_before_spawn_starts_no_child() {
    let execution = Execution::new();
    execution.cancel_and_wait().unwrap();
    assert!(matches!(
        execute(&task("test:hang"), &execution),
        Outcome::Cancelled
    ));
    assert!(execution.process.lock().unwrap().child.is_none());
}

#[test]
fn cancellation_interrupts_io_and_exit_wait_and_reaps_worker() {
    for mode in ["no_read", "hang", "response_then_hang"] {
        let execution = Arc::new(Execution::new());
        let task = if mode == "no_read" {
            task(&"x".repeat(4 * 1024 * 1024))
        } else {
            task("unused")
        };
        let worker_execution = Arc::clone(&execution);
        let command = command(mode);
        let (send, receive) = mpsc::channel();
        let worker = thread::spawn(move || {
            send.send(execute_with_command(&task, &worker_execution, command))
                .unwrap();
        });
        wait_for_worker(&execution);
        let phase = match mode {
            "no_read" => Phase::WritingTask,
            "hang" => Phase::ReadingResult,
            _ => Phase::WaitingForExit,
        };
        let deadline = Instant::now() + TIMEOUT;
        while execution.phase.load(Ordering::SeqCst) != phase as u8 {
            assert!(
                Instant::now() < deadline,
                "Worker did not reach target phase: {mode}"
            );
            thread::sleep(Duration::from_millis(1));
        }
        execution.cancel_and_wait().unwrap();
        assert!(execution.process.lock().unwrap().child.is_none(), "{mode}");
        assert!(
            matches!(receive.recv_timeout(TIMEOUT).unwrap(), Outcome::Cancelled),
            "{mode}"
        );
        worker.join().unwrap();
        execution.cancel_and_wait().unwrap(); // Repeated cancellation is harmless.
    }
}

#[test]
fn worker_failures_are_reported_and_reaped() {
    for mode in [
        "exit",
        "exit_after_result",
        "truncated",
        "malformed",
        "wrong_task",
    ] {
        let execution = Execution::new();
        assert!(
            matches!(
                execute_with_command(&task("unused"), &execution, command(mode)),
                Outcome::Failed(_)
            ),
            "{mode}"
        );
        assert!(execution.process.lock().unwrap().child.is_none(), "{mode}");
    }
    let execution = Execution::new();
    assert!(matches!(
        execute_with_command(
            &task("unused"),
            &execution,
            Command::new("/nonexistent/maestro-worker")
        ),
        Outcome::Failed(_)
    ));
}

#[test]
fn result_socket_handles_logs_and_results_larger_than_socket_buffer() {
    for mode in ["logs", "large"] {
        let execution = Execution::new();
        let Outcome::Completed(result) =
            execute_with_command(&task("unused"), &execution, command(mode))
        else {
            panic!("Worker did not complete: {mode}");
        };
        let result = json::parse(&result).unwrap();
        if mode == "large" {
            assert_eq!(result["data"].as_str().unwrap().len(), 2 * 1024 * 1024);
        } else {
            assert_eq!(result["counts"]["0"].as_u32(), Some(1));
            assert!(execution.logs().contains("Native diagnostic on stdout"));
            assert!(execution.logs().contains("Native diagnostic on stderr"));
        }
        assert!(execution.process.lock().unwrap().child.is_none());
    }
}

#[test]
fn native_execute_and_estimate_use_worker() {
    let mut task = task("OPENQASM 2.0; qreg q[1]; creg c[1]; x q[0]; measure q[0]->c[0];");
    for task_type in [TaskType::Execute, TaskType::Estimate] {
        task.task_type = task_type;
        task.observables = "Z".to_owned();
        let Outcome::Completed(result) = execute(&task, &Execution::new()) else {
            panic!("Native worker did not complete");
        };
        let result = json::parse(&result).unwrap();
        match task.task_type {
            TaskType::Request | TaskType::ValidateRequest => unreachable!("legacy test"),
            TaskType::Execute => assert_eq!(result["counts"]["1"].as_u32(), Some(1)),
            TaskType::Estimate => {
                assert!((result["expectation_values"][0].as_f64().unwrap() + 1.0).abs() < 1e-9)
            }
        }
    }
}

#[test]
fn native_fixed_selection_preserves_method_and_multiple_shots() {
    for method in ["statevector", "matrix_product_state"] {
        for explicit_selection in [false, true] {
            let mut request = json::object! {
                schema_version: 2, operation: "execute",
                circuit: {
                    num_qubits: 2, num_clbits: 2,
                    source: "OPENQASM 2.0; qreg q[2]; creg c[2]; h q[0]; cx q[0],q[1]; measure q->c;",
                },
                simulator: { backend: "qcsim", method: method },
                execution: { shots: 4096, seed: 123 },
            };
            if explicit_selection {
                request["simulator"]["selection"] = "fixed".into();
            }
            let mut task = task(&request.dump());
            task.task_type = TaskType::Request;
            let Outcome::Completed(result) = execute(&task, &Execution::new()) else {
                panic!("Fixed native worker did not complete");
            };
            let result = json::parse(&result).unwrap();
            assert_eq!(result["ok"], true);
            assert_eq!(result["execution_metadata"]["backend"], "qcsim");
            assert_eq!(result["execution_metadata"]["method"], method);
            assert_eq!(result["execution_metadata"]["selection"], "fixed");
            assert_eq!(result["shots"].as_u64(), Some(4096));
            assert_eq!(result["counts"].len(), 2);
            let zeros = result["counts"]["00"].as_u64().unwrap();
            let ones = result["counts"]["11"].as_u64().unwrap();
            assert_eq!(zeros + ones, 4096);
            assert!(zeros > 1024 && ones > 1024);
        }
    }
}

#[test]
fn truncated_protocol_frame_is_rejected() {
    let data = [0, 0, 0, 0, 0, 0, 0, 20, b'{'];
    let error = protocol::read_message(&mut data.as_slice()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn results_travel_unescaped_up_to_the_result_limit() {
    // A quote-heavy native result at the limit. Escaping it as a JSON string, as
    // protocol version 1 did, would have nearly doubled its frame.
    let limit = crate::maestro::api::MAX_RESULT_BYTES;
    let mut result = String::with_capacity(limit + 1);
    result.push_str(r#"{"k":""#);
    while result.len() < limit - 2 {
        result.push_str(r#"\""#);
    }
    result.push_str(r#""}"#);
    assert_eq!(result.len(), limit);
    let task = task("unused");
    let mut data = Vec::new();
    protocol::write_result(&mut data, &task, &result, None).unwrap();
    assert!(data.len() < limit + 1024);
    let decoded = protocol::read_result(&mut data.as_slice(), &task).unwrap();
    assert!(decoded.result == result);

    // One byte more is refused before anything is written.
    result.insert(limit - 2, ' ');
    let mut oversized = Vec::new();
    assert!(protocol::write_result(&mut oversized, &task, &result, None).is_err());
    assert!(oversized.is_empty());
    // A result frame advertising more is rejected without reading it.
    let mut small = Vec::new();
    protocol::write_result(&mut small, &task, "{}", None).unwrap();
    small.truncate(small.len() - 10);
    small.extend_from_slice(&(limit as u64 + 1).to_be_bytes());
    let Err(error) = protocol::read_result(&mut small.as_slice(), &task) else {
        panic!("An oversized result frame was accepted");
    };
    assert!(error.to_string().contains("exceeds size limit"), "{error}");
}

#[test]
fn oversized_results_reach_the_parent_as_structured_failures() {
    let task = task("unused");
    let result = "x".repeat(crate::maestro::api::MAX_RESULT_BYTES + 1);
    let mut data = Vec::new();
    assert!(send_result(&mut data, &task, &result, None).is_err());
    let decoded = protocol::read_result(&mut data.as_slice(), &task).unwrap();
    let error = diagnostics::native_error(&decoded.result).unwrap();
    let value = json::parse(&error.to_string()).unwrap();
    assert_eq!(value["error"]["code"], "invalid_input");
    assert!(
        value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("exceeds the server result limit"),
        "{value}"
    );
}

#[test]
fn log_capture_is_bounded_and_frames_reject_oversized_lengths() {
    let execution = Execution::new();
    assert!(matches!(
        execute_with_command(&task("unused"), &execution, command("flood")),
        Outcome::Completed(_)
    ));
    let mut record = diagnostics::TaskDiagnostics::new(json::object! {});
    record.output = Arc::clone(&execution.logs);
    let document = record.document(1, 1);
    assert_eq!(document["logs_truncated"], true);
    assert!(document["logs_omitted_bytes"].as_u64().unwrap() > 0);
    assert!(execution.logs().contains("Startup diagnostic"));
    assert!(execution.logs().contains("Late failure diagnostic"));
    assert!(
        execution
            .logs()
            .contains("[... Maestro log output truncated ...]")
    );
    assert!(execution.logs().len() < 64 * 1024 + 100);
    let header = (64 * 1024 * 1024 + 1u64).to_be_bytes();
    assert!(protocol::read_message(&mut &header[..]).is_err());
}

#[test]
fn structured_native_failure_reaches_parent_without_losing_details() {
    let mut task = task(r#"{"schema_version":2,"operation":"unknown"}"#);
    task.task_type = TaskType::Request;
    let Outcome::Failed(error) = execute(&task, &Execution::new()) else {
        panic!("Invalid native request unexpectedly succeeded");
    };
    let value = json::parse(&error.to_string()).unwrap();
    assert_eq!(
        value["error"]["code"].as_str(),
        Some("unsupported_capability")
    );
}

#[test]
fn isolated_validation_returns_input_errors_without_killing_the_parent() {
    let valid = r#"{"schema_version":2,"operation":"execute","circuit":{"format":"instructions","num_qubits":2,"source":[]}}"#;
    assert_eq!(
        json::parse(&validate_request(valid).unwrap()).unwrap()["ok"].as_bool(),
        Some(true)
    );
    let invalid = valid.replace(
        "\"source\":[]",
        "\"source\":[{\"name\":\"cx\",\"qubits\":[0]}]",
    );
    assert_eq!(
        json::parse(&validate_request(&invalid).unwrap()).unwrap()["error"]["code"].as_str(),
        Some("invalid_input")
    );
    assert!(validate_with_command(valid, command("exit")).is_err());
    assert!(validate_with_command(valid, command("truncated")).is_err());
    // A fresh validation still works after child process/protocol failures.
    assert_eq!(
        json::parse(&validate_request(valid).unwrap()).unwrap()["ok"].as_bool(),
        Some(true)
    );
}

pub(super) fn pause_worker(phase: &str, cancelled: Option<&AtomicBool>) {
    if std::env::var("MAESTRO_TEST_WORKER_PAUSE").as_deref() != Ok(phase) {
        return;
    }
    eprintln!("TEST_WORKER_PAUSE {phase}");
    while cancelled.is_none_or(|flag| !flag.load(Ordering::SeqCst)) {
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn mpi_cancellation_before_launch_and_after_cleanup_is_acknowledged() {
    for pause in ["before_ready", "before_read", "after_task", "after_cleanup"] {
        let mut task = task(
            r#"{"schema_version":2,"operation":"execute","launch":{"profile":"absent","ranks":2}}"#,
        );
        task.task_type = TaskType::Request;
        // An unknown profile fails before launching. The after_cleanup case
        // still exercises writing a completed native-error frame to a closed socket.
        let execution = Arc::new(Execution::new());
        let other = execution.clone();
        let mut command = worker_command().unwrap();
        command
            .env("MAESTRO_TEST_PRODUCTION_WORKER", "1")
            .env("MAESTRO_TEST_WORKER_PAUSE", pause)
            .env_remove("MAESTRO_MPI_PROFILES");
        let worker = thread::spawn(move || execute_with_command(&task, &other, command));
        let deadline = Instant::now() + TIMEOUT;
        while !execution
            .logs()
            .contains(&format!("TEST_WORKER_PAUSE {pause}"))
        {
            assert!(Instant::now() < deadline, "{pause}: {}", execution.logs());
            thread::sleep(Duration::from_millis(1));
        }
        execution
            .cancel_and_wait()
            .unwrap_or_else(|error| panic!("{pause}: {error}"));
        assert!(matches!(worker.join().unwrap(), Outcome::Cancelled));
        assert!(execution.process.lock().unwrap().child.is_none());
    }
}

#[test]
fn mpi_cleanup_failure_exit_is_never_acknowledged_as_success() {
    let mut process = Process {
        child: Some(
            Command::new("/bin/sh")
                .args(["-c", "exit 75"])
                .process_group(0)
                .spawn()
                .unwrap(),
        ),
        socket: None,
        mpi: true,
        ready: true,
        cleanup_error: None,
    };
    assert!(terminate(&mut process).is_err());
    assert!(process.child.is_none());
    assert!(terminate(&mut process).is_err());
}

#[test]
fn protocol_failure_and_unconfirmed_worker_exit_are_both_retained() {
    let mut task = task(r#"{"schema_version":2,"launch":{"profile":"test","ranks":2}}"#);
    task.task_type = TaskType::Request;
    let execution = Execution::new();
    let Outcome::Failed(error) =
        execute_with_command(&task, &execution, command("wrong_task_cleanup_failure"))
    else {
        panic!("Double failure was not reported");
    };
    let value = json::parse(&error.to_string()).unwrap();
    assert_eq!(value["error"]["code"], "cleanup_unconfirmed");
    assert!(
        value["error"]["causes"][0]["message"]
            .as_str()
            .unwrap()
            .contains("different task")
    );
    assert!(execution.cancel_and_wait().is_err());
}

#[test]
fn previous_signalling_failure_does_not_prevent_cleanup_of_a_live_child() {
    let mut process = Process {
        child: Some(
            Command::new("/bin/sleep")
                .arg("30")
                .process_group(0)
                .spawn()
                .unwrap(),
        ),
        cleanup_error: Some((
            io::ErrorKind::PermissionDenied,
            "earlier signalling failure".into(),
        )),
        ..Process::default()
    };
    terminate(&mut process).unwrap();
    assert!(process.child.is_none());
    assert!(process.cleanup_error.is_none());
}

#[test]
fn production_supervisor_preserves_native_failure_when_slurm_cleanup_fails() {
    let file = std::env::temp_dir().join(format!(
        "maestro-double-failure-{}.json",
        std::process::id()
    ));
    let script = r#"exec 0<&-; echo 'MAESTRO_STEP_V2 123.4'; echo 'MAESTRO_RESULT_V2 {"ok":false,"error":{"code":"original_failure","message":"native reason"}}'; exec sleep 30"#;
    std::fs::write(
        &file,
        json::object! {version:1, profiles:{test:{
            kind:"srun", launcher:"/bin/sh", worker:"/bin/true", args:["-c",script],
            scancel:"/bin/false", max_ranks:2
        }}}
        .dump(),
    )
    .unwrap();
    let mut task = task(
        &json::object! {schema_version:2, operation:"execute",
        launch:{profile:"test", ranks:2}, padding:"x".repeat(1024*1024)}
        .dump(),
    );
    task.task_type = TaskType::Request;
    let execution = Execution::new();
    let mut command = worker_command().unwrap();
    command
        .env("MAESTRO_TEST_PRODUCTION_WORKER", "1")
        .env("MAESTRO_MPI_PROFILES", &file);
    let outcome = execute_with_command(&task, &execution, command);
    std::fs::remove_file(file).unwrap();
    let Outcome::Failed(error) = outcome else {
        panic!("Cleanup failure was lost");
    };
    let value = json::parse(&error.to_string()).unwrap();
    assert_eq!(value["error"]["code"], "cleanup_unconfirmed");
    assert_eq!(
        value["error"]["causes"][0]["causes"][0]["code"],
        "original_failure"
    );
    assert_eq!(
        value["error"]["causes"][0]["causes"][0]["message"],
        "native reason"
    );
    assert!(error.to_string().contains("scancel rejected"));
    assert!(execution.cancel_and_wait().is_err());
    assert!(execution.process.lock().unwrap().child.is_none());
}

#[test]
fn mpi_supervisor_cancellation_tracks_running_and_cleaned_launchers() {
    for mode in ["running", "cleaned", "failed_scancel"] {
        let file =
            std::env::temp_dir().join(format!("maestro-cancel-{}-{mode}.json", std::process::id()));
        let script = if mode == "cleaned" {
            r#"cat >/dev/null; printf 'MAESTRO_RESULT_V2 {"schema_version":2,"ok":true}\n'"#
        } else {
            r#"trap 'exit 0' TERM; cat >/dev/null; printf 'MAESTRO_STEP_V2 123.4\n'; echo MPI_LAUNCHER_READY >&2; while :; do sleep 0.01; done"#
        };
        std::fs::write(&file, json::object! {version:1, profiles:{test:{
            kind:if mode == "failed_scancel" {"srun"} else {"mpirun"},
            launcher:"/bin/sh", worker:"/bin/true", args:["-c",script], scancel:"/bin/false", max_ranks:2
        }}}.dump()).unwrap();
        let mut task = task(
            r#"{"schema_version":2,"operation":"execute","launch":{"profile":"test","ranks":2}}"#,
        );
        task.task_type = TaskType::Request;
        let execution = Arc::new(Execution::new());
        let other = execution.clone();
        let mut command = worker_command().unwrap();
        command
            .env("MAESTRO_TEST_PRODUCTION_WORKER", "1")
            .env("MAESTRO_MPI_PROFILES", &file);
        if mode == "cleaned" {
            command.env("MAESTRO_TEST_WORKER_PAUSE", "after_cleanup");
        }
        let worker = thread::spawn(move || execute_with_command(&task, &other, command));
        let marker = if mode == "cleaned" {
            "TEST_WORKER_PAUSE after_cleanup"
        } else {
            "MPI_LAUNCHER_READY"
        };
        let deadline = Instant::now() + TIMEOUT;
        while !execution.logs().contains(marker) {
            if Instant::now() >= deadline {
                let _ = execution.cancel_and_wait();
                panic!("{mode}: {}", execution.logs());
            }
            thread::sleep(Duration::from_millis(1));
        }
        let cancelled = execution.cancel_and_wait();
        assert_eq!(
            cancelled.is_ok(),
            mode != "failed_scancel",
            "{mode}: {cancelled:?}"
        );
        let _ = worker.join().unwrap();
        assert!(execution.process.lock().unwrap().child.is_none());
        std::fs::remove_file(file).unwrap();
    }
}
