//! Runner lifecycle tests using real, controllable worker processes.

use super::*;
use crate::maestro::sessions::Session;
use std::time::{Duration, Instant};

struct RunningRunner {
    runner: Arc<Runner>,
    thread: Option<thread::JoinHandle<i32>>,
}

impl RunningRunner {
    fn new() -> Self {
        let runner = Runner::new();
        let thread = Some(Runner::run(Arc::clone(&runner)));
        Self { runner, thread }
    }
}

impl Drop for RunningRunner {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            self.runner.stop().unwrap();
            thread.join().unwrap();
        }
    }
}

fn submit(session: &Session, runner: &Arc<Runner>, qasm: &str) -> u32 {
    let id = session.initiate_task();
    session.set_task_qubits(id, 1);
    session.set_task_sim_type(id, 1);
    session.set_task_method(id, 1);
    session.set_task_qasm(id, qasm.to_owned());
    session.set_task_options(id, r#"{"shots":1}"#.to_owned());
    assert_eq!(
        session.handle_request(vec![&id.to_string(), "EXECUTE"], Arc::clone(runner)),
        "OK\n"
    );
    id
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "Timed out waiting for runner");
        thread::sleep(Duration::from_millis(1));
    }
}

fn wait_for_execution(
    runner: &Runner,
    session_id: u32,
    task_id: u32,
) -> Arc<maestro::worker::Execution> {
    let mut execution = None;
    wait_until(|| {
        execution = runner
            .active_task
            .lock()
            .unwrap()
            .as_ref()
            .filter(|active| active.session_id == session_id && active.task_id == task_id)
            .map(|active| Arc::clone(&active.execution));
        execution.is_some()
    });
    let execution = execution.unwrap();
    maestro::worker::tests::wait_for_worker(&execution);
    execution
}

const SIMPLE_QASM: &str = "OPENQASM 2.0; qreg q[1]; creg c[1]; x q[0]; measure q[0]->c[0];";

#[test]
fn running_task_cancellation_allows_next_task_to_complete() {
    let running = RunningRunner::new();
    let runner = &running.runner;
    let session = Session::new(1);
    let cancelled = submit(&session, runner, "test:hang");
    let next = submit(&session, runner, SIMPLE_QASM);
    let execution = wait_for_execution(runner, 1, cancelled);

    assert_eq!(
        session.handle_request(vec![&cancelled.to_string(), "CANCEL"], Arc::clone(runner)),
        "OK YES\n"
    );
    assert!(execution.is_cancelled());
    assert!(!runner.is_running(1, cancelled));
    assert!(!session.is_sent_to_runner(cancelled));
    wait_until(|| runner.is_task_result_available(1, next));
    assert!(!runner.is_task_result_available(1, cancelled));
    assert!(!runner.is_task_result_failed(1, next));
    assert!(!runner.get_result(1, next).unwrap().result.is_empty());
}

fn event_states(document: &json::JsonValue) -> Vec<&str> {
    document["events"]
        .members()
        .map(|event| {
            assert!(event["timestamp_unix_ms"].as_u64().unwrap() > 0);
            event["state"].as_str().unwrap()
        })
        .collect()
}

#[test]
fn cancelled_task_logs_include_shutdown_output_and_survive_until_session_cleanup() {
    let running = RunningRunner::new();
    let runner = &running.runner;
    let manager = maestro::sessions::SessionManager::new(100);
    let id = manager.create_session();
    let session = manager.get_session(id).unwrap();
    let task = session
        .submit_request(
            json::object! {
                test_worker: "logs_on_cancel", operation: "execute",
                simulator: {backend: "gpu", method: "statevector"},
                launch: {profile: "test", ranks: 2},
            }
            .dump(),
            runner,
        )
        .unwrap();
    wait_until(|| {
        runner.diagnostics(id, task)["logs"]
            .as_str()
            .unwrap()
            .contains("Worker ready for cancellation")
    });
    assert_eq!(
        event_states(&runner.diagnostics(id, task)),
        ["queued", "started"]
    );
    assert!(session.cancel_task(Arc::clone(runner), task).unwrap());
    let response = maestro::api::handle(
        &json::object! {version:2, command:"logs", session_id:id, task_id:task}.dump(),
        &manager,
        runner,
    );
    let document = json::parse(response.strip_prefix("OK ").unwrap().trim()).unwrap();
    assert_eq!(document["ok"], true);
    assert_eq!(document["session_id"], id);
    assert_eq!(document["task_id"], task);
    assert_eq!(event_states(&document), ["queued", "started", "cancelled"]);
    assert!(
        document["logs"]
            .as_str()
            .unwrap()
            .contains("Final output during cancellation")
    );
    assert!(
        document["logs"]
            .as_str()
            .unwrap()
            .contains("Worker ready for cancellation")
    );
    assert_eq!(document["logs_truncated"], true);
    assert_eq!(document["context"]["simulator"]["backend"], "gpu");
    assert_eq!(document["context"]["launch"]["profile"], "test");
    assert!(document["error"].is_null());
    assert!(!runner.is_task_result_available(id, task));
    assert!(manager.remove_session(id, Arc::clone(runner)).unwrap());
    // Wait for another task to finish so any late publication has also run.
    let other = Session::new(id + 1);
    let next = submit(&other, runner, SIMPLE_QASM);
    wait_until(|| runner.is_task_result_available(id + 1, next));
    assert!(!runner.has_diagnostics(id, task));
}

#[test]
fn failed_cancellation_retains_cleanup_error_without_a_success_event() {
    let mut running = RunningRunner::new();
    let runner = &running.runner;
    let session = Session::new(1);
    let task = session
        .submit_request(
            json::object! {
                test_worker: "logs_on_cancel_failure", launch: {profile: "test", ranks: 2},
            }
            .dump(),
            runner,
        )
        .unwrap();
    wait_until(|| {
        runner.diagnostics(1, task)["logs"]
            .as_str()
            .unwrap()
            .contains("Worker ready for cancellation")
    });
    let cancelled = session.cancel_task(Arc::clone(runner), task);
    // The worker has exited, but intentionally did not confirm remote cleanup.
    // Stop/join before assertions; the normal guard requires successful cleanup.
    let _ = runner.stop();
    running.thread.take().unwrap().join().unwrap();
    assert!(cancelled.is_err());
    let document = runner.diagnostics(1, task);
    assert_eq!(
        event_states(&document),
        ["queued", "started", "cancellation_failed"]
    );
    assert_eq!(document["error"]["code"], "cleanup_unconfirmed");
    assert_eq!(document["error"]["cleanup_confirmed"], false);
    assert!(
        document["logs"]
            .as_str()
            .unwrap()
            .contains("Final output during cancellation")
    );
    assert!(session.is_sent_to_runner(task));
}

#[test]
fn completed_and_failed_task_history_survives_result_consumption() {
    let running = RunningRunner::new();
    let runner = &running.runner;
    let session = Session::new(1);
    for (input, state) in [("test:logs", "completed"), ("test:malformed", "failed")] {
        let task = submit(&session, runner, input);
        wait_until(|| runner.is_task_result_available(1, task));
        let before = runner.diagnostics(1, task);
        assert_eq!(event_states(&before), ["queued", "started", state]);
        assert_eq!(before["events_truncated"], false);
        assert_eq!(before["logs_truncated"], false);
        assert_eq!(before["context"]["operation"], "execute");
        assert_eq!(before["context"]["simulator"]["type_id"], 1);
        assert_eq!(before["failure_message"].is_null(), state == "completed");
        assert!(runner.get_result(1, task).is_some());
        assert_eq!(runner.diagnostics(1, task), before);
    }
}

#[test]
fn shutdown_terminates_active_worker_and_rejects_more_work() {
    let running = RunningRunner::new();
    let runner = &running.runner;
    let session = Session::new(1);
    let active = submit(&session, runner, "test:hang");
    let queued = submit(&session, runner, SIMPLE_QASM);
    let execution = wait_for_execution(runner, 1, active);
    runner.stop().unwrap();
    assert!(execution.is_cancelled());
    assert!(!runner.is_running(1, active));
    assert!(runner.tasks_queue.lock().unwrap().is_empty());
    assert!(!runner.is_task_result_available(1, queued));
    assert!(!runner.add_task(maestro::worker::tests::task(SIMPLE_QASM)));
}

#[test]
fn session_deletion_and_expiration_cancel_workers_and_close_retained_sessions() {
    for expire in [false, true] {
        let running = RunningRunner::new();
        let runner = &running.runner;
        let manager = maestro::sessions::SessionManager::new(0);
        let id = manager.create_session();
        let session = manager.get_session(id).unwrap();
        let active = submit(&session, runner, "test:hang");
        let queued = submit(&session, runner, SIMPLE_QASM);
        wait_for_execution(runner, id, active);
        if expire {
            manager.tick_sessions(Arc::clone(runner));
        } else {
            assert!(manager.remove_session(id, Arc::clone(runner)).unwrap());
        }
        assert!(!manager.session_exists(id));
        assert!(!runner.is_running(id, active));
        assert!(!runner.is_task_result_available(id, active));
        assert!(!runner.is_task_result_available(id, queued));
        assert!(!runner.has_diagnostics(id, active));
        assert!(!runner.has_diagnostics(id, queued));
        assert!(runner.tasks_queue.lock().unwrap().is_empty());
        assert_eq!(
            session.handle_request(vec![&queued.to_string(), "EXECUTE"], Arc::clone(runner)),
            "ERROR Session is closed\n"
        );
        let other_session = Session::new(id + 1);
        let other = submit(&other_session, runner, SIMPLE_QASM);
        wait_until(|| runner.is_task_result_available(id + 1, other));
        assert!(!runner.is_task_result_failed(id + 1, other));
        assert!(!runner.has_diagnostics(id, active));
    }
}

#[test]
fn cancellation_prevents_late_result_publication() {
    let runner = Runner::new();
    let task = maestro::worker::tests::task(SIMPLE_QASM);
    let execution = Arc::new(maestro::worker::Execution::new());
    *runner.active_task.lock().unwrap() = Some(ActiveTask {
        session_id: task.session_id,
        task_id: task.task_id,
        execution: Arc::clone(&execution),
    });
    runner.remove_task(task.session_id, task.task_id).unwrap();
    // A result already buffered by the runner must not undo cancellation.
    runner.finish_task(
        &task,
        &execution,
        maestro::worker::Outcome::Completed("{}".to_owned()),
    );
    assert!(!runner.is_task_result_available(task.session_id, task.task_id));
    assert!(matches!(
        maestro::worker::execute(&task, &execution),
        maestro::worker::Outcome::Cancelled
    ));
}

#[test]
fn immediate_cancellation_covers_dequeue_and_worker_startup() {
    let running = RunningRunner::new();
    let runner = &running.runner;
    let session = Session::new(1);
    for _ in 0..64 {
        let id = submit(&session, runner, "test:hang");
        assert_eq!(
            session.handle_request(vec![&id.to_string(), "CANCEL"], Arc::clone(runner)),
            "OK YES\n"
        );
        assert!(!runner.is_running(1, id));
        assert!(!runner.is_task_result_available(1, id));
    }
    let next = submit(&session, runner, SIMPLE_QASM);
    wait_until(|| runner.is_task_result_available(1, next));
    assert!(!runner.is_task_result_failed(1, next));
}
