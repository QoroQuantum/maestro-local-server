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
    if mode == "no_read" {
        hang();
    }
    if mode == "exit" {
        std::process::exit(42);
    }
    let mut socket = worker_socket()?;
    let mut task = protocol::read_task(&mut socket)?;
    if let Some(requested) = task.qasm.strip_prefix("test:") {
        mode = requested.to_owned();
    }
    if mode == "hang" {
        hang();
    }
    if mode == "truncated" {
        socket.write_all(&1000_u64.to_be_bytes())?;
        socket.write_all(b"{")?;
        return Ok(());
    }
    if mode == "wrong_task" {
        task.task_id += 1;
    }
    let result = match mode.as_str() {
        "malformed" => "not JSON".to_owned(),
        "large" => json::object! {data: "x".repeat(2 * 1024 * 1024)}.dump(),
        "response_then_hang" | "exit_after_result" | "wrong_task" | "logs" => {
            println!("Native diagnostic on stdout");
            eprintln!("Native diagnostic on stderr");
            r#"{"counts":{"0":1}}"#.to_owned()
        }
        _ => task.execute(),
    };
    protocol::write_result(&mut socket, &task, &result)?;
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
            TaskType::Execute => assert_eq!(result["counts"]["1"].as_u32(), Some(1)),
            TaskType::Estimate => {
                assert!((result["expectation_values"][0].as_f64().unwrap() + 1.0).abs() < 1e-9)
            }
        }
    }
}

#[test]
fn truncated_protocol_frame_is_rejected() {
    let data = [0, 0, 0, 0, 0, 0, 0, 20, b'{'];
    let error = protocol::read_message(&mut data.as_slice()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
}
