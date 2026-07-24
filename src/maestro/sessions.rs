use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::Arc;
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
	id : AtomicU32,
	initiated_tasks : Mutex<HashMap<u32, InitiatedTask>>,
	timer_counter : AtomicU32,
    task_counter : AtomicU32,
}


#[derive(Debug)]
pub struct SessionManager {
	sessions : Mutex<HashMap<u32, Arc<Session>>>,
	session_counter : AtomicU32,
	time_limit : AtomicU32,
}

impl InitiatedTask {
    fn new(task_id: u32) -> Self {
        Self {
            task_id,
            task_type: runner::TaskType::Execute,
            qasm : "".to_string(),
            observables : "".to_string(),
            nr_qubits : 0,
            sim_type : 0,
            method : 0,
            options : "".to_string(),
        }
    }

    fn is_valid(&self) -> bool {
        !self.qasm.is_empty() && self.nr_qubits > 0 && (matches!(self.task_type, runner::TaskType::Execute) || (matches!(self.task_type, runner::TaskType::Estimate) && !self.observables.is_empty()))
    }
}

impl Session {
    pub fn new(session_id : u32) -> Self {
        Self {
            id : AtomicU32::new(session_id),
            initiated_tasks : Mutex::new(HashMap::new()),
            timer_counter : AtomicU32::new(0),
            task_counter : AtomicU32::new(0),
        }
    }

    pub fn tick(&self) {
        self.timer_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn reset_timer(&self) {
        self.timer_counter.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn timed_out(&self, time_limit : u32) -> bool {
        self.timer_counter.load(std::sync::atomic::Ordering::SeqCst) > time_limit
    }

    pub fn get_id(&self) -> u32 {
        self.id.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn is_task_initiated(&self, task_id : u32) -> bool {
        self.initiated_tasks.lock().unwrap().contains_key(&task_id)
    }

    pub fn initiate_task(&self) -> u32 {
        let task_id = self.task_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let task = InitiatedTask::new(task_id);

        self.initiated_tasks.lock().unwrap().insert(task_id, task);
        task_id
    }

    pub fn set_task_type(&self, task_id : u32, task_type : runner::TaskType) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.task_type = task_type;
            return true;
        }
        
        false
    }

    pub fn set_task_qasm(&self, task_id : u32, qasm : String) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.qasm = qasm;
            return true;
        }
        
        false
    }

    pub fn set_task_observables(&self, task_id : u32, observables : String) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.observables = observables;
            return true;
        }
        
        false
    }

    pub fn set_task_qubits(&self, task_id : u32, nr_qubits : i32) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.nr_qubits = nr_qubits;
            return true;
        }
        
        false
    }

    pub fn set_task_sim_type(&self, task_id : u32, sim_type : i32) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.sim_type = sim_type;
            return true;
        }
        
        false
    }

    pub fn set_task_method(&self, task_id : u32, method : i32) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.method = method;
            return true;
        }
        
        false
    }

    pub fn set_task_options(&self, task_id : u32, options : String) -> bool {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get_mut(&task_id) {
            task.options = options;
            return true;
        }
        
        false
    }

    pub fn get_task_for_execution(&self, task_id : u32) -> Option<runner::Task> {
        if let Some(task) = self.initiated_tasks.lock().unwrap().get(&task_id) {
            if task.is_valid() {
                // remove it from the initiated tasks list
                self.initiated_tasks.lock().unwrap().remove(&task_id);

                let mut task_type = runner::TaskType::Execute;
                if matches!(task_type, runner::TaskType::Estimate) {
                    task_type = runner::TaskType::Estimate;
                }

                return Some(runner::Task::new(
                    self.get_id(),
                    task.task_id,
                    task_type,
                    task.qasm.clone(),
                    task.observables.clone(),
                    task.nr_qubits,
                    task.sim_type,
                    task.method,
                    task.options.clone()
                ));
            }
        }
        None
    }

}

impl SessionManager {
    pub fn new(time_limit : u32) -> Arc<Self> {
        let manager = Self {
            sessions : Mutex::new(HashMap::new()),
            session_counter : AtomicU32::new(0),
            time_limit : AtomicU32::new(time_limit),
        };

        Arc::new(manager)
    }

    pub fn create_session(&self) -> u32 {
        let session_id = self.session_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let session = Arc::new(Session::new(session_id));
        self.sessions.lock().unwrap().insert(session_id, session);
        session_id
    }
    
    pub fn get_session(&self, session_id : u32) -> Option<Arc<Session>> {
        self.sessions.lock().unwrap().get(&session_id).map(|s| Arc::clone(s))
    }
    
    pub fn remove_session(&self, session_id : u32) {
        self.sessions.lock().unwrap().remove(&session_id);
    }
    
    pub fn tick_sessions(&self) {
        let time_limit = self.time_limit.load(std::sync::atomic::Ordering::SeqCst);
        let mut sessions_to_remove = Vec::new();
        for (session_id, session) in self.sessions.lock().unwrap().iter() {
            session.tick();
            if session.timed_out(time_limit) {
                sessions_to_remove.push(*session_id);
            }
        }
        for session_id in sessions_to_remove {
            self.remove_session(session_id);
        }
    }
}