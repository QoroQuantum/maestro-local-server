use std::collections::HashMap; // for results
use std::collections::VecDeque; // for tasks queue
use std::thread;

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

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
    pub logs: String,
}

impl TaskResult {
    pub fn new(session_id: u32, task_id: u32, result: String) -> TaskResult {
        Self {
            session_id,
            task_id,
            result,
            error: None,
            logs: String::new(),
        }
    }
}

#[derive(Debug)]
struct ActiveTask {
    session_id: u32,
    task_id: u32,
    execution: Arc<maestro::worker::Execution>,
}

#[derive(Debug)]
pub struct Runner {
    tasks_queue: Mutex<VecDeque<Task>>,
    results_map: Mutex<HashMap<u64, TaskResult>>,
    consumed_diagnostics: Mutex<HashMap<u64, (String, Option<String>)>>,
    stop: AtomicBool,
    // Lock order: tasks_queue -> active_task -> results_map. Register a task
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
            consumed_diagnostics: Mutex::new(HashMap::new()),
            stop: std::sync::atomic::AtomicBool::new(false),
            active_task: Mutex::new(None),
        };

        Arc::new(myself)
    }

    pub fn run(runner: Arc<Runner>) -> std::thread::JoinHandle<i32> {
        thread::spawn(move || {
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
                        *runner.active_task.lock().unwrap() = Some(ActiveTask {
                            session_id: task.session_id,
                            task_id: task.task_id,
                            execution: Arc::clone(&execution),
                        });
                        (task, execution)
                    })
                };

                if let Some((task, execution)) = task_opt {
                    // Previous in-process execution, retained for comparison:
                    // let result = task.execute();
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

    fn finish_task(
        &self,
        task: &Task,
        execution: &Arc<maestro::worker::Execution>,
        outcome: maestro::worker::Outcome,
    ) {
        let mut active = self.active_task.lock().unwrap();
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
                record.logs = execution.logs();
                self.results_map
                    .lock()
                    .unwrap()
                    .insert((task.session_id as u64) << 32 | task.task_id as u64, record);
            }
        }
        *active = None;
    }

    pub fn stop(&self) -> std::io::Result<()> {
        let execution = {
            let mut queue = self.tasks_queue.lock().unwrap();
            self.stop.store(true, Ordering::SeqCst);
            queue.clear();
            let active = self.active_task.lock().unwrap();
            active.as_ref().map(|active| {
                active.execution.request_cancel();
                Arc::clone(&active.execution)
            })
        };
        self.cancel_execution(execution)
    }

    pub fn add_task(&self, task: Task) -> bool {
        let mut queue = self.tasks_queue.lock().unwrap();
        if self.stop.load(Ordering::SeqCst) {
            return false;
        }
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
        let mut results = self.results_map.lock().unwrap();

        let key = (session_id as u64) << 32 | task_id as u64;
        let result = results.remove(&key);
        if let Some(record) = result.as_ref() {
            self.consumed_diagnostics
                .lock()
                .unwrap()
                .insert(key, (record.logs.clone(), record.error.clone()));
        }
        result
    }

    pub fn diagnostics(&self, session_id: u32, task_id: u32) -> (String, Option<String>) {
        let active = self.active_task.lock().unwrap();
        if let Some(active) = active
            .as_ref()
            .filter(|a| a.session_id == session_id && a.task_id == task_id)
        {
            return (active.execution.logs(), None);
        }
        let results = self.results_map.lock().unwrap();
        results
            .get(&((session_id as u64) << 32 | task_id as u64))
            .map(|r| (r.logs.clone(), r.error.clone()))
            .or_else(|| {
                self.consumed_diagnostics
                    .lock()
                    .unwrap()
                    .get(&((session_id as u64) << 32 | task_id as u64))
                    .cloned()
            })
            .unwrap_or_default()
    }

    pub fn has_consumed_diagnostics(&self, session_id: u32, task_id: u32) -> bool {
        self.consumed_diagnostics
            .lock()
            .unwrap()
            .contains_key(&((session_id as u64) << 32 | task_id as u64))
    }
    pub fn clear_session_diagnostics(&self, session_id: u32) {
        self.consumed_diagnostics
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
        let execution = {
            let mut queue = self.tasks_queue.lock().unwrap();
            queue.retain(|task| !(task.session_id == session_id && task.task_id == task_id));
            let active = self.active_task.lock().unwrap();
            let execution = active
                .as_ref()
                .filter(|active| active.session_id == session_id && active.task_id == task_id)
                .map(|active| {
                    active.execution.request_cancel();
                    Arc::clone(&active.execution)
                });
            self.results_map
                .lock()
                .unwrap()
                .remove(&((session_id as u64) << 32 | task_id as u64));
            execution
        };
        // Process termination and reaping never hold the queue or result locks.
        self.cancel_execution(execution)
    }

    fn cancel_execution(
        &self,
        execution: Option<Arc<maestro::worker::Execution>>,
    ) -> std::io::Result<()> {
        if let Some(execution) = execution {
            execution.cancel_and_wait()?;
            let mut active = self.active_task.lock().unwrap();
            if active
                .as_ref()
                .is_some_and(|active| Arc::ptr_eq(&active.execution, &execution))
            {
                *active = None;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests_runner {
    use super::*;
    use crate::maestro::sessions::Session;
    use serial_test::serial;
    use std::sync::Barrier;

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
