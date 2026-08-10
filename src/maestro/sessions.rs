use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicU32;

use crate::maestro::runner;

#[derive(Debug)]
struct InitiatedTask {
    pub task_id: u32,
    task_type: runner::TaskType,
    qasm: String,
    observables: String,
    nr_qubits: i32,
    sim_type: i32,
    method: i32,
    options: String,
}

#[derive(Debug)]
pub struct Session {
    id: AtomicU32,
    initiated_tasks: Mutex<HashMap<u32, InitiatedTask>>,
    sent_to_runner: Mutex<HashSet<u32>>,
    timer_counter: AtomicU32,
    task_counter: AtomicU32,
}

#[derive(Debug)]
pub struct SessionManager {
    sessions: Mutex<HashMap<u32, Arc<Session>>>,
    session_counter: AtomicU32,
    time_limit: AtomicU32,
}

impl InitiatedTask {
    fn new(task_id: u32) -> Self {
        Self {
            task_id,
            task_type: runner::TaskType::Execute,
            qasm: "".to_string(),
            observables: "".to_string(),
            nr_qubits: 0,
            sim_type: 0,
            method: 0,
            options: "".to_string(),
        }
    }

    fn is_valid(&self) -> bool {
        !self.qasm.is_empty()
            && self.nr_qubits > 0
            && (matches!(self.task_type, runner::TaskType::Execute)
                || (matches!(self.task_type, runner::TaskType::Estimate)
                    && !self.observables.is_empty()))
    }
}

impl Session {
    pub fn new(session_id: u32) -> Self {
        Self {
            id: AtomicU32::new(session_id),
            initiated_tasks: Mutex::new(HashMap::new()),
            sent_to_runner: Mutex::new(HashSet::new()),
            timer_counter: AtomicU32::new(0),
            task_counter: AtomicU32::new(0),
        }
    }

    pub fn tick(&self) {
        self.timer_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn reset_timer(&self) {
        self.timer_counter
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn timed_out(&self, time_limit: u32) -> bool {
        self.timer_counter.load(std::sync::atomic::Ordering::SeqCst) > time_limit
    }

    pub fn get_id(&self) -> u32 {
        self.id.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn initiate_task(&self) -> u32 {
        let task_id = self
            .task_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let task = InitiatedTask::new(task_id);

        self.initiated_tasks.lock().unwrap().insert(task_id, task);
        task_id
    }

    pub fn is_task_initiated(&self, task_id: u32) -> bool {
        self.initiated_tasks.lock().unwrap().contains_key(&task_id)
    }

    pub fn set_task_type(&self, task_id: u32, task_type: runner::TaskType) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.task_type = task_type;
            return true;
        }

        false
    }

    pub fn set_task_qasm(&self, task_id: u32, qasm: String) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.qasm = qasm;
            return true;
        }

        false
    }

    pub fn set_task_observables(&self, task_id: u32, observables: String) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.observables = observables;
            return true;
        }

        false
    }

    pub fn set_task_qubits(&self, task_id: u32, nr_qubits: i32) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.nr_qubits = nr_qubits;
            return true;
        }

        false
    }

    pub fn set_task_sim_type(&self, task_id: u32, sim_type: i32) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.sim_type = sim_type;
            return true;
        }

        false
    }

    pub fn set_task_method(&self, task_id: u32, method: i32) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.method = method;
            return true;
        }

        false
    }

    pub fn set_task_options(&self, task_id: u32, options: String) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.options = options;
            return true;
        }

        false
    }

    pub fn get_task_for_execution(&self, task_id: u32) -> Option<runner::Task> {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get(&task_id)
            && !task.is_valid()
        {
            return None;
        }

        let mut result: Option<runner::Task> = None;

        if let Some(task) = self.initiated_tasks.lock().unwrap().remove(&task_id)
            && task.is_valid()
        {
            result = Some(runner::Task::new(
                self.get_id(),
                task.task_id,
                task.task_type,
                task.qasm,
                task.observables,
                task.nr_qubits,
                task.sim_type,
                task.method,
                task.options,
            ));
        }

        if result.is_some() {
            self.sent_to_runner.lock().unwrap().insert(task_id);
        }

        result
    }

    pub fn execution_finished(&self, task_id: u32) {
        self.sent_to_runner.lock().unwrap().remove(&task_id);
    }

    pub fn is_sent_to_runner(&self, task_id: u32) -> bool {
        self.sent_to_runner.lock().unwrap().contains(&task_id)
    }

    fn remove_tasks(&self, runner: Arc<runner::Runner>) {
        // walk over sent_to_runner and remove tasks from runner
        for task_id in self.sent_to_runner.lock().unwrap().iter() {
            runner.remove_task(self.get_id(), *task_id);
        }
    }

    pub fn cancel_task(&self, runner: Arc<runner::Runner>, task_id: u32) -> bool {
        if self.is_sent_to_runner(task_id) || runner.is_running(self.get_id(), task_id) {
            runner.remove_task(self.get_id(), task_id);
            self.execution_finished(task_id);
            return true;
        } else {
            // might be in 'initiated_tasks' but not yet sent to runner, so remove it from there
            if self
                .initiated_tasks
                .lock()
                .unwrap()
                .remove(&task_id)
                .is_some()
            {
                return true;
            }
        }
        false
    }

    // SESSION <session_id> TASK ... commands
    // only what's following TASK in the command is present in request
    // CREATE
    // an id followed by EXISTS, VALID, PENDING, RUNNING, FINISHED, FAILED, EXECUTE, GET_RESULTS, SET_TYPE, SET_QUBITS, SET_SIM_TYPE, SET_METHOD, SET_OBSERVABLES, SET_QASM, SET_OPTIONS
    // NOTE: After GET_RESULTS the task will be removed

    pub fn handle_request(&self, request: Vec<&str>, runner: Arc<runner::Runner>) -> String {
        self.reset_timer();

        if request[0] == "CREATE" {
            let task_id = self.initiate_task();
            return format!("OK {}\n", task_id);
        }

        // extract task id
        let task_id = request[0].parse::<u32>();
        if task_id.is_err() {
            return String::from("ERROR Invalid task ID\n");
        }

        let task_id = task_id.unwrap();

        let task_command = request[1];

        //eprintln!("Session {}: Handling task command: {} for task ID: {}", self.get_id(), task_command, task_id);

        match task_command {
            "EXISTS" => {
                if self.is_task_initiated(task_id) || self.is_sent_to_runner(task_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }
            "CANCEL" => {
                if self.cancel_task(runner, task_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }
            "VALID" => {
                if self.is_sent_to_runner(task_id) {
                    return String::from("OK YES\n");
                }

                if let Some(task) = self.initiated_tasks.lock().unwrap().get(&task_id) {
                    if task.is_valid() {
                        return String::from("OK YES\n");
                    } else {
                        return String::from("OK NO\n");
                    }
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "PENDING" => {
                if self.is_sent_to_runner(task_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }
            "RUNNING" => {
                if runner.is_running(self.get_id(), task_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }
            "FINISHED" => {
                if runner.is_task_result_available(self.get_id(), task_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }
            "FAILED" => {
                if runner.is_task_result_failed(self.get_id(), task_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }
            "EXECUTE" => {
                if let Some(task) = self.get_task_for_execution(task_id) {
                    // send the task to the runner
                    runner.add_task(task);
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task is not valid or does not exist\n");
                }
            }
            "GET_RESULTS" => {
                if runner.is_task_result_available(self.get_id(), task_id) {
                    if let Some(result) = runner.get_result(self.get_id(), task_id) {
                        // get_result already removed the task from the runner
                        self.execution_finished(task_id); // this removes the task from the sent_to_runner list

                        return format!("OK {}\n", result.result);
                    } else {
                        return String::from("ERROR Could not retrieve result\n");
                    }
                } else {
                    return String::from("ERROR Result not available\n");
                }
            }
            "SET_TYPE" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing task type\n");
                }
                let task_type = request[2];
                let task_type_enum = match task_type {
                    "EXECUTE" => runner::TaskType::Execute,
                    "ESTIMATE" => runner::TaskType::Estimate,
                    _ => return String::from("ERROR Invalid task type\n"),
                };
                if self.set_task_type(task_id, task_type_enum) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "SET_QUBITS" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing number of qubits\n");
                }
                let nr_qubits = request[2].parse::<i32>();
                if nr_qubits.is_err() {
                    return String::from("ERROR Invalid number of qubits\n");
                }
                if self.set_task_qubits(task_id, nr_qubits.unwrap()) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "SET_SIM_TYPE" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing simulator type\n");
                }
                let sim_type = request[2].parse::<i32>();
                if sim_type.is_err() {
                    return String::from("ERROR Invalid simulator type\n");
                }
                if self.set_task_sim_type(task_id, sim_type.unwrap()) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "SET_METHOD" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing method\n");
                }
                let method = request[2].parse::<i32>();
                if method.is_err() {
                    return String::from("ERROR Invalid method\n");
                }
                if self.set_task_method(task_id, method.unwrap()) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "SET_OBSERVABLES" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing observables\n");
                }
                let observables = request[2..].join(" ");
                if self.set_task_observables(task_id, observables) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "SET_QASM" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing QASM\n");
                }
                let qasm = request[2..].join(" ");
                if self.set_task_qasm(task_id, qasm) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            "SET_OPTIONS" => {
                if request.len() < 3 {
                    return String::from("ERROR Missing options\n");
                }
                let options = request[2..].join(" ");
                if self.set_task_options(task_id, options) {
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Task does not exist\n");
                }
            }
            _ => {}
        }

        "ERROR Unknown request\n".to_string()
    }
}

impl SessionManager {
    pub fn new(time_limit: u32) -> Arc<Self> {
        let manager = Self {
            sessions: Mutex::new(HashMap::new()),
            session_counter: AtomicU32::new(0),
            time_limit: AtomicU32::new(time_limit),
        };

        Arc::new(manager)
    }

    pub fn create_session(&self) -> u32 {
        let session_id = self
            .session_counter
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let session = Arc::new(Session::new(session_id));
        self.sessions.lock().unwrap().insert(session_id, session);
        session_id
    }

    pub fn get_session(&self, session_id: u32) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .unwrap()
            .get(&session_id)
            .map(Arc::clone)
    }

    pub fn session_exists(&self, session_id: u32) -> bool {
        self.sessions.lock().unwrap().contains_key(&session_id)
    }

    pub fn remove_session(&self, session_id: u32, runner: Arc<runner::Runner>) {
        let session = self.get_session(session_id);

        self.sessions.lock().unwrap().remove(&session_id);

        if let Some(session) = session {
            session.remove_tasks(runner);
        }
    }

    pub fn tick_sessions(&self, runner: Arc<runner::Runner>) {
        let time_limit = self.time_limit.load(std::sync::atomic::Ordering::SeqCst);
        let mut sessions_to_remove = Vec::new();
        for (session_id, session) in self.sessions.lock().unwrap().iter() {
            session.tick();
            if session.timed_out(time_limit) {
                sessions_to_remove.push(*session_id);
            }
        }
        for session_id in sessions_to_remove {
            self.remove_session(session_id, Arc::clone(&runner));
        }
    }

    // SESSION CREATE
    // SESSION <session_id> EXISTS
    // SESSION <session_id> DELETE
    // SESSION <session_id> TASK ... commands
    pub fn handle_request(&self, request: String, runner: Arc<runner::Runner>) -> String {
        if request.starts_with("CREATE") {
            let session_id = self.create_session();
            format!("OK {}\n", session_id)
        } else {
            // extract all chars up to whitespace as session_id
            let parts: Vec<&str> = request.split_whitespace().collect();

            if parts.len() < 2 {
                return String::from("ERROR Invalid request\n");
            }

            let session_id = parts[0].parse::<u32>();
            if session_id.is_err() {
                return String::from("ERROR Invalid session ID\n");
            }

            let session_id = session_id.unwrap();

            if parts[1] == "DELETE" {
                if self.session_exists(session_id) {
                    self.remove_session(session_id, runner);
                    return String::from("OK\n");
                } else {
                    return String::from("ERROR Session does not exist\n");
                }
            } else if parts[1] == "EXISTS" {
                if self.session_exists(session_id) {
                    return String::from("OK YES\n");
                } else {
                    return String::from("OK NO\n");
                }
            }

            if self.session_exists(session_id) {
                if parts[1] == "TASK" && parts.len() >= 3 {
                    let session = self.get_session(session_id).unwrap();
                    session.handle_request(parts[2..].to_vec(), runner)
                } else {
                    String::from("ERROR Unknown command\n")
                }
            } else {
                String::from("ERROR Unknown session\n")
            }
        }
    }
}
