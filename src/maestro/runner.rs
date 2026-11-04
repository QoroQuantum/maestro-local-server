use std::collections::HashMap; // for results
use std::collections::VecDeque; // for tasks queue
use std::thread;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use super::diagnostics::{self, TaskDiagnostics};
use crate::maestro;

#[derive(Debug)]
pub enum TaskType {
    Execute,
    Estimate,
    Request,
    ValidateRequest,
}

#[derive(Debug)]
pub struct Task {
    pub session_id: u32,
    pub task_id: u32,
    pub task_type: TaskType,
    pub qasm: String,
    pub observables: String,
    pub nr_qubits: i32,
    pub sim_type: i32,
    pub method: i32,
    pub options: String,
}

impl Task {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: u32,
        task_id: u32,
        task_type: TaskType,
        qasm: String,
        observables: String,
        nr_qubits: i32,
        sim_type: i32,
        method: i32,
        options: String,
    ) -> Task {
        Self {
            session_id,
            task_id,
            task_type,
            qasm,
            observables,
            nr_qubits,
            sim_type,
            method,
            options,
        }
    }

    pub fn execute(&mut self) -> String {
        if matches!(self.task_type, TaskType::Request) {
            return maestro::simulators::run_request(&self.qasm);
        }
        if matches!(self.task_type, TaskType::ValidateRequest) {
            return maestro::simulators::validate_request(&self.qasm);
        }
        // Native execution is called only by worker mode, in the child process.
        let mut simulator = maestro::simulators::SimpleSimulator::new(self.nr_qubits);
        let _res = simulator.remove_all_optimization_simulators_and_add(self.sim_type, self.method);

        let result = match self.task_type {
            TaskType::Execute => simulator.execute(self.qasm.clone(), self.options.clone()),
            TaskType::Request | TaskType::ValidateRequest => unreachable!(),
            TaskType::Estimate => simulator.estimate(
                self.qasm.clone(),
                self.observables.clone(),
                self.options.clone(),
            ),
        };

        simulator.destroy();

        result
    }
}

#[derive(Debug)]
pub struct TaskResult {
    pub session_id: u32,
    pub task_id: u32,
    pub result: String,
    pub error: Option<String>,
}

impl TaskResult {
    pub fn new(session_id: u32, task_id: u32, result: String) -> TaskResult {
        Self {
            session_id,
            task_id,
            result,
            error: None,
        }
    }
}

#[derive(Debug, Clone)]
struct ActiveTask {
    session_id: u32,
    task_id: u32,
    execution: Arc<maestro::worker::Execution>,
}

/// A removal started by `Runner::begin_removal`, holding the worker (if the
/// task was running) that `Runner::finish_removal` waits for.
#[must_use]
#[derive(Debug)]
pub struct Removal(Option<ActiveTask>);

#[derive(Debug)]
pub struct Runner {
    tasks_queue: Mutex<VecDeque<Task>>,
    results_map: Mutex<HashMap<u64, TaskResult>>,
    task_diagnostics: Mutex<HashMap<u64, TaskDiagnostics>>,
    stop: AtomicBool,
    running: AtomicBool,
    // Lock order: tasks_queue -> active_task -> results_map -> task_diagnostics.
    // Diagnostic output locks come last. Register a task
    // before releasing the queue lock, including while its worker is starting.
    active_task: Mutex<Option<ActiveTask>>,
}

impl Runner {
    pub fn new() -> Arc<Runner> {
        let tasks_queue = VecDeque::<Task>::new();
        let tasks_mutex = Mutex::<VecDeque<Task>>::new(tasks_queue);

        let results_map = HashMap::<u64, TaskResult>::new();
        let results_mutex = Mutex::<HashMap<u64, TaskResult>>::new(results_map);

        let myself = Self {
            tasks_queue: tasks_mutex,
            results_map: results_mutex,
            task_diagnostics: Mutex::new(HashMap::new()),
            stop: std::sync::atomic::AtomicBool::new(false),
            active_task: Mutex::new(None),
            running: AtomicBool::new(false),
        };

        Arc::new(myself)
    }

    pub fn run(runner: Arc<Runner>) -> std::thread::JoinHandle<i32> {
        thread::spawn(move || {
            // The guard clears liveness on normal exit and panic unwinding.
            struct Activity<'a>(&'a AtomicBool);
            impl Drop for Activity<'_> {
                fn drop(&mut self) {
                    self.0.store(false, Ordering::SeqCst);
                }
            }
            runner.running.store(true, Ordering::SeqCst);
            let _activity = Activity(&runner.running);
            loop {
                // Dequeue and register together, so cancellation cannot miss
                // a task between leaving the queue and spawning its worker.
                let task_opt = {
                    let mut queue = runner.tasks_queue.lock().unwrap();
                    if runner.stop.load(Ordering::SeqCst) {
                        break;
                    }
                    queue.pop_front().map(|task| {
                        let execution = Arc::new(maestro::worker::Execution::new());
                        let mut active = runner.active_task.lock().unwrap();
                        let mut records = runner.task_diagnostics.lock().unwrap();
                        if let Some(record) =
                            records.get_mut(&task_key(task.session_id, task.task_id))
                        {
                            record.output = Arc::clone(&execution.logs);
                            record.event("started");
                        }
                        *active = Some(ActiveTask {
                            session_id: task.session_id,
                            task_id: task.task_id,
                            execution: Arc::clone(&execution),
                        });
                        (task, execution)
                    })
                };

                if let Some((task, execution)) = task_opt {
                    let outcome = maestro::worker::execute(&task, &execution);
                    runner.finish_task(&task, &execution, outcome);
                } else {
                    // No tasks, sleep for a while
                    thread::sleep(std::time::Duration::from_millis(1));
                }
            }

            0
        })
    }

    /// A consistent queue/active snapshot; this does not probe devices or renew sessions.
    pub fn status(&self) -> json::JsonValue {
        let queue = self
            .tasks_queue
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let active = self
            .active_task
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let poisoned = self.tasks_queue.is_poisoned()
            || self.active_task.is_poisoned()
            || self.results_map.is_poisoned()
            || self.task_diagnostics.is_poisoned();
        let running = self.running.load(Ordering::SeqCst);
        let stopping = self.stop.load(Ordering::SeqCst);
        let healthy = running && !poisoned;
        let reason = if poisoned {
            Some("runner_state_poisoned")
        } else if stopping {
            Some("shutting_down")
        } else if !running {
            Some("runner_not_running")
        } else {
            None
        };
        json::object! {
            accepting_jobs: healthy && !stopping,
            healthy: healthy,
            busy: active.is_some() || !queue.is_empty(),
            pending_job_count: queue.len(),
            status_reason: reason,
            health_scope: "runner",
        }
    }

    fn finish_task(
        &self,
        task: &Task,
        execution: &Arc<maestro::worker::Execution>,
        outcome: maestro::worker::Outcome,
    ) {
        let mut active = self.active_task.lock().unwrap();
        let key = task_key(task.session_id, task.task_id);
        // Even after cancellation cleared the active slot, the worker may have
        // a structured cleanup failure to preserve. Never recreate a record
        // after session cleanup removed it.
        if execution.is_cancelled()
            && let maestro::worker::Outcome::Failed(error) = &outcome
            && let Some(record) = self.task_diagnostics.lock().unwrap().get_mut(&key)
        {
            record.error = Some(error.to_string());
        }
        if !active
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(&active.execution, execution))
        {
            return; // Cancellation already cleared this execution.
        }
        // Cancellation requests and result publication use this same lock.
        // Once cancellation wins, even a complete buffered result is discarded.
        if !execution.is_cancelled() {
            let mut failure = None;
            let result = match outcome {
                maestro::worker::Outcome::Completed(result) => Some(result),
                maestro::worker::Outcome::Failed(error) => {
                    eprintln!(
                        "Maestro task {}:{} failed: {error}",
                        task.session_id, task.task_id
                    );
                    failure = Some(error.to_string());
                    Some(String::new()) // Existing FAILED protocol semantics.
                }
                maestro::worker::Outcome::Cancelled => None,
            };
            if let Some(result) = result {
                let mut record = TaskResult::new(task.session_id, task.task_id, result);
                record.error = failure;
                let mut results = self.results_map.lock().unwrap();
                if let Some(diagnostic) = self.task_diagnostics.lock().unwrap().get_mut(&key) {
                    diagnostic.native_build = execution.native_build();
                    if record.error.is_none() {
                        diagnostic.record_result(&record.result);
                    }
                    diagnostic.event(if record.error.is_some() {
                        "failed"
                    } else {
                        "completed"
                    });
                    diagnostic.error = record.error.clone();
                }
                results.insert(key, record);
            }
        }
        *active = None;
    }

    pub fn stop(&self) -> std::io::Result<()> {
        let execution = {
            let mut queue = self.tasks_queue.lock().unwrap();
            self.stop.store(true, Ordering::SeqCst);
            for task in queue.drain(..) {
                self.mark_cancelled(task.session_id, task.task_id);
            }
            let active = self.active_task.lock().unwrap();
            active.as_ref().map(|active| {
                active.execution.request_cancel();
                active.clone()
            })
        };
        self.cancel_execution(execution)
    }

    pub fn add_task(&self, task: Task) -> bool {
        let mut queue = self.tasks_queue.lock().unwrap();
        if self.stop.load(Ordering::SeqCst) {
            return false;
        }
        let mut diagnostic = TaskDiagnostics::new(diagnostics::task_context(&task));
        diagnostic.event("queued");
        self.task_diagnostics
            .lock()
            .unwrap()
            .insert(task_key(task.session_id, task.task_id), diagnostic);
        queue.push_back(task);
        true
    }

    pub fn is_task_result_available(&self, session_id: u32, task_id: u32) -> bool {
        let results = self.results_map.lock().unwrap();
        results.contains_key(&((session_id as u64) << 32 | (task_id as u64)))
    }

    pub fn is_task_result_failed(&self, session_id: u32, task_id: u32) -> bool {
        let results = self.results_map.lock().unwrap();
        results
            .get(&((session_id as u64) << 32 | (task_id as u64)))
            .is_some_and(|result| result.result.is_empty())
    }

    pub fn get_result(&self, session_id: u32, task_id: u32) -> Option<TaskResult> {
        self.results_map
            .lock()
            .unwrap()
            .remove(&task_key(session_id, task_id))
    }

    pub fn diagnostics(&self, session_id: u32, task_id: u32) -> json::JsonValue {
        self.task_diagnostics
            .lock()
            .unwrap()
            .get(&task_key(session_id, task_id))
            .map(|record| record.document(session_id, task_id))
            .unwrap_or_else(|| TaskDiagnostics::default().document(session_id, task_id))
    }

    pub fn has_diagnostics(&self, session_id: u32, task_id: u32) -> bool {
        self.task_diagnostics
            .lock()
            .unwrap()
            .contains_key(&((session_id as u64) << 32 | task_id as u64))
    }
    pub fn clear_session_diagnostics(&self, session_id: u32) {
        self.task_diagnostics
            .lock()
            .unwrap()
            .retain(|key, _| (*key >> 32) as u32 != session_id);
    }

    pub fn is_running(&self, session_id: u32, task_id: u32) -> bool {
        self.active_task
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|active| active.session_id == session_id && active.task_id == task_id)
    }

    pub fn remove_task(&self, session_id: u32, task_id: u32) -> std::io::Result<()> {
        let removal = self.begin_removal(session_id, task_id);
        self.finish_removal(removal)
    }

    /// Dequeues the task, discards its result and asks an active worker to
    /// stop. Nothing here waits, so callers may hold their own locks; release
    /// them before `finish_removal`, which waits for the worker to exit (up to
    /// the MPI cleanup budget).
    pub fn begin_removal(&self, session_id: u32, task_id: u32) -> Removal {
        let execution = {
            let mut queue = self.tasks_queue.lock().unwrap();
            let queued = queue
                .iter()
                .any(|task| task.session_id == session_id && task.task_id == task_id);
            queue.retain(|task| !(task.session_id == session_id && task.task_id == task_id));
            let active = self.active_task.lock().unwrap();
            let execution = active
                .as_ref()
                .filter(|active| active.session_id == session_id && active.task_id == task_id)
                .map(|active| {
                    active.execution.request_cancel();
                    active.clone()
                });
            self.results_map
                .lock()
                .unwrap()
                .remove(&((session_id as u64) << 32 | task_id as u64));
            if queued {
                self.mark_cancelled(session_id, task_id);
            }
            execution
        };
        Removal(execution)
    }

    /// Waits until a worker stopped by `begin_removal` has exited and been
    /// reaped. Process termination and reaping never hold the queue or result locks.
    pub fn finish_removal(&self, removal: Removal) -> std::io::Result<()> {
        self.cancel_execution(removal.0)
    }

    fn cancel_execution(&self, execution: Option<ActiveTask>) -> std::io::Result<()> {
        if let Some(task) = execution {
            let result = task.execution.cancel_and_wait();
            let mut active = self.active_task.lock().unwrap();
            if let Some(record) = self
                .task_diagnostics
                .lock()
                .unwrap()
                .get_mut(&task_key(task.session_id, task.task_id))
            {
                match &result {
                    Ok(()) => record.event("cancelled"),
                    Err(error) => {
                        let earlier = record.error.clone().map(std::io::Error::other);
                        record.error = Some(
                            diagnostics::with_causes(
                                std::io::Error::new(error.kind(), error.to_string()),
                                "cleanup_unconfirmed",
                                &earlier.as_ref().into_iter().collect::<Vec<_>>(),
                            )
                            .to_string(),
                        );
                        record.event("cancellation_failed");
                    }
                }
            }
            if result.is_ok()
                && active
                    .as_ref()
                    .is_some_and(|active| Arc::ptr_eq(&active.execution, &task.execution))
            {
                *active = None;
            }
            return result;
        }
        Ok(())
    }

    fn mark_cancelled(&self, session_id: u32, task_id: u32) {
        if let Some(record) = self
            .task_diagnostics
            .lock()
            .unwrap()
            .get_mut(&task_key(session_id, task_id))
        {
            record.event("cancelled");
        }
    }

    pub fn record_unsubmitted_cancellation(&self, session_id: u32, task_id: u32) {
        let mut record = TaskDiagnostics::default();
        record.event("cancelled");
        self.task_diagnostics
            .lock()
            .unwrap()
            .insert(task_key(session_id, task_id), record);
    }
}

fn task_key(session_id: u32, task_id: u32) -> u64 {
    (session_id as u64) << 32 | task_id as u64
}

#[cfg(test)]
mod tests_runner {
    use super::*;
    use crate::maestro::sessions::Session;
    use serial_test::serial;
    use std::sync::Barrier;

    #[test]
    fn status_tracks_liveness_queue_active_work_and_shutdown() {
        let runner = Runner::new();
        assert_eq!(runner.status()["accepting_jobs"], false);
        runner.running.store(true, Ordering::SeqCst);
        assert_eq!(runner.status()["healthy"], true);
        assert_eq!(runner.status()["busy"], false);
        let task = maestro::worker::tests::task("");
        runner.tasks_queue.lock().unwrap().push_back(task);
        assert_eq!(runner.status()["pending_job_count"], 1);
        assert_eq!(runner.status()["busy"], true);
        let task = runner.tasks_queue.lock().unwrap().pop_front().unwrap();
        *runner.active_task.lock().unwrap() = Some(ActiveTask {
            session_id: task.session_id,
            task_id: task.task_id,
            execution: Arc::new(maestro::worker::Execution::new()),
        });
        assert_eq!(runner.status()["pending_job_count"], 0);
        assert_eq!(runner.status()["busy"], true);
        runner.active_task.lock().unwrap().take();
        assert_eq!(runner.status()["busy"], false);
        runner.stop.store(true, Ordering::SeqCst);
        assert_eq!(runner.status()["accepting_jobs"], false);
        assert_eq!(runner.status()["status_reason"], "shutting_down");
        runner.running.store(false, Ordering::SeqCst);
        assert_eq!(runner.status()["healthy"], false);
    }

    #[test]
    fn status_reports_poisoned_runner_without_panicking() {
        let runner = Runner::new();
        let clone = Arc::clone(&runner);
        assert!(
            thread::spawn(move || {
                let _queue = clone.tasks_queue.lock().unwrap();
                panic!("test runner failure");
            })
            .join()
            .is_err()
        );
        assert_eq!(runner.status()["healthy"], false);
        assert_eq!(runner.status()["status_reason"], "runner_state_poisoned");
    }

    #[test]
    fn resolved_seed_history_survives_result_consumption_until_session_cleanup() {
        let runner = Runner::new();
        let mut task = maestro::worker::tests::task("");
        task.task_type = TaskType::Request;
        let key = task_key(task.session_id, task.task_id);
        let execution = Arc::new(maestro::worker::Execution::new());
        *runner.active_task.lock().unwrap() = Some(ActiveTask {
            session_id: task.session_id,
            task_id: task.task_id,
            execution: Arc::clone(&execution),
        });
        runner
            .task_diagnostics
            .lock()
            .unwrap()
            .insert(key, TaskDiagnostics::default());
        let result = json::object! {operation: "execute", seed: u64::MAX, noise: {seed: 0}}.dump();
        runner.finish_task(
            &task,
            &execution,
            maestro::worker::Outcome::Completed(result.clone()),
        );
        let before = runner.diagnostics(task.session_id, task.task_id);
        assert_eq!(before["seeds"][0]["seed"].as_u64(), Some(u64::MAX));
        assert_eq!(before["seeds"][0]["noise_seed"], 0);
        assert_eq!(
            runner
                .get_result(task.session_id, task.task_id)
                .unwrap()
                .result,
            result
        );
        assert!(!runner.is_task_result_available(task.session_id, task.task_id));
        assert_eq!(runner.diagnostics(task.session_id, task.task_id), before);
        runner.clear_session_diagnostics(task.session_id);
        assert!(!runner.has_diagnostics(task.session_id, task.task_id));
    }

    #[test]
    fn cancelled_task_is_not_enqueued() {
        for cancel_first in [true, false] {
            let runner = Runner::new();
            let session = Session::new(1);
            let task_id = session.initiate_task();
            session.set_task_qubits(task_id, 1);
            session.set_task_qasm(task_id, "OPENQASM 2.0; qreg q[1];".to_string());
            let task_id = task_id.to_string();

            if cancel_first {
                assert_eq!(
                    session.handle_request(vec![&task_id, "CANCEL"], Arc::clone(&runner)),
                    "OK YES\n"
                );
                assert_eq!(
                    session.handle_request(vec![&task_id, "EXECUTE"], Arc::clone(&runner)),
                    "ERROR Task is not valid or does not exist\n"
                );
            } else {
                assert_eq!(
                    session.handle_request(vec![&task_id, "EXECUTE"], Arc::clone(&runner)),
                    "OK\n"
                );
                assert_eq!(
                    session.handle_request(vec![&task_id, "CANCEL"], Arc::clone(&runner)),
                    "OK YES\n"
                );
            }

            assert!(runner.tasks_queue.lock().unwrap().is_empty());
            assert_eq!(
                session.handle_request(vec![&task_id, "EXISTS"], Arc::clone(&runner)),
                "OK NO\n"
            );
        }
    }

    #[test]
    fn waiting_for_cancellation_cleanup_does_not_block_the_session() {
        use std::time::{Duration, Instant};
        let runner = Runner::new();
        let session = Session::new(1);
        let task_id = session.submit_request(String::new(), &runner).unwrap();
        let task = runner.tasks_queue.lock().unwrap().pop_front().unwrap();
        // A running MPI task whose supervisor takes a second to clean up.
        let execution = Arc::new(maestro::worker::tests::slow_mpi_execution(
            Duration::from_secs(1),
        ));
        *runner.active_task.lock().unwrap() = Some(ActiveTask {
            session_id: task.session_id,
            task_id,
            execution: Arc::clone(&execution),
        });
        let id = task_id.to_string();
        thread::scope(|scope| {
            let cancel =
                scope.spawn(|| session.handle_request(vec![&id, "CANCEL"], Arc::clone(&runner)));
            let deadline = Instant::now() + Duration::from_secs(5);
            while !execution.is_cancelled() {
                assert!(Instant::now() < deadline, "Cancellation did not start");
                thread::sleep(Duration::from_millis(1));
            }
            // The task stays listed and running until its cleanup is confirmed,
            // and other commands are answered meanwhile.
            let start = Instant::now();
            assert_eq!(
                session.handle_request(vec![&id, "EXISTS"], Arc::clone(&runner)),
                "OK YES\n"
            );
            assert_eq!(
                session.handle_request(vec![&id, "RUNNING"], Arc::clone(&runner)),
                "OK YES\n"
            );
            assert!(
                start.elapsed() < Duration::from_millis(500),
                "Session commands waited {:?} for worker cleanup",
                start.elapsed()
            );
            assert_eq!(cancel.join().unwrap(), "OK YES\n");
        });
        assert_eq!(
            session.handle_request(vec![&id, "EXISTS"], Arc::clone(&runner)),
            "OK NO\n"
        );
        assert!(!runner.is_running(1, task_id));
        assert_eq!(
            runner.diagnostics(1, task_id)["events"]
                .members()
                .last()
                .map(|event| event["state"].to_string()),
            Some("cancelled".to_string())
        );
    }

    #[test]
    fn concurrent_execute_and_cancel_do_not_leave_queued_task() {
        // Keep the runner stopped so this exercises enqueue/cancel ordering,
        // without racing the native simulator or requiring running-task cancellation.
        let runner = Runner::new();
        let session = Session::new(1);

        for _ in 0..1024 {
            let task_id = session.initiate_task();
            session.set_task_qubits(task_id, 1);
            session.set_task_qasm(task_id, "OPENQASM 2.0; qreg q[1];".to_string());
            let task_id_str = task_id.to_string();
            let start = Barrier::new(2);

            let (execute_response, cancel_response) = thread::scope(|scope| {
                let execute = scope.spawn(|| {
                    start.wait();
                    session.handle_request(vec![&task_id_str, "EXECUTE"], Arc::clone(&runner))
                });
                start.wait();
                let cancel_response =
                    session.handle_request(vec![&task_id_str, "CANCEL"], Arc::clone(&runner));
                (execute.join().unwrap(), cancel_response)
            });

            assert_eq!(
                cancel_response, "OK YES\n",
                "Task {task_id} was lost during submission"
            );
            assert!(
                execute_response == "OK\n"
                    || execute_response == "ERROR Task is not valid or does not exist\n"
            );
            assert!(
                runner.tasks_queue.lock().unwrap().is_empty(),
                "Task {task_id} was enqueued after cancellation"
            );
            assert!(!session.is_task_initiated(task_id));
            assert!(!session.is_sent_to_runner(task_id));
        }
    }

    #[test]
    #[serial]
    fn runner_test() {
        //let obj = maestro::simulators::SimpleSimulator::GetMaestroObject();
        let runner = Runner::new();
        let handle = Runner::run(Arc::clone(&runner));

        let qasm = "OPENQASM 2.0; \
            creg c[3]; \
            qreg q[3]; \
            x q[0]; \
            cx q[0],q[1]; \
            measure q[0]->c[0]; \
            measure q[1]->c[1];"
            .to_string();

        // Add a task to the queue
        let task = Task::new(
            1,
            1,
            TaskType::Execute,
            qasm.clone(),
            "".to_string(),
            5,
            1,
            1,
            "".to_string(),
        );
        runner.add_task(task);

        // Wait for the task to be processed
        std::thread::sleep(std::time::Duration::from_secs(3));
        // Check the results
        {
            let results = runner.results_map.lock().unwrap();
            assert!(
                results.contains_key(&(1u64 << 32 | 1u64)),
                "Result not found for task"
            );
            let result = results.get(&(1u64 << 32 | 1u64)).unwrap();
            assert!(!result.result.is_empty(), "Result is empty");
        }
        // Stop the runner
        runner.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        handle.join().unwrap();
    }

    #[test]
    #[serial]
    fn is_task_result_failed_test() {
        let runner = Runner::new();

        // no result yet: neither available nor failed
        assert!(!runner.is_task_result_available(2, 1));
        assert!(!runner.is_task_result_failed(2, 1));

        // an empty result signals a failure
        runner
            .results_map
            .lock()
            .unwrap()
            .insert((2u64 << 32) | 1u64, TaskResult::new(2, 1, "".to_string()));
        assert!(runner.is_task_result_available(2, 1));
        assert!(runner.is_task_result_failed(2, 1));

        // a non-empty result is not a failure
        runner.results_map.lock().unwrap().insert(
            (2u64 << 32) | 2u64,
            TaskResult::new(2, 2, "{\"counts\": {}}".to_string()),
        );
        assert!(runner.is_task_result_available(2, 2));
        assert!(!runner.is_task_result_failed(2, 2));
    }
}

#[cfg(test)]
mod process_tests;
