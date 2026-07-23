//use std::io::{Error, ErrorKind};
use std::collections::VecDeque; // for tasks queue
use std::collections::HashMap; // for results
use std::thread;



//use std::sync::mpsc; 
use std::sync::Mutex;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::maestro;

#[derive(Debug)]
pub enum TaskType {
    Execute,
    Estimate,
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

pub fn new(session_id: u32, task_id : u32, task_type: TaskType, qasm: String, observables : String, nr_qubits: i32, sim_type: i32, method : i32, options: String) -> Task {
    Self {
        session_id,
        task_id,
        task_type,
        qasm,
        observables,
        nr_qubits,
        sim_type,
        method,
        options
    }
}

pub fn execute(&mut self) -> String {
    let mut simulator = maestro::simulators::SimpleSimulator::new(self.nr_qubits);
    let _res = simulator.RemoveAllOptimizationSimulatorsAndAdd(self.sim_type, self.method);

    let result = match self.task_type {
        TaskType::Execute => simulator.Execute(self.qasm.clone(), self.options.clone()),
        TaskType::Estimate => simulator.Estimate(self.qasm.clone(), self.observables.clone(), self.options.clone())
    };

    simulator.destroy();

    result
}

}

#[derive(Debug)]
pub struct TaskResult {
    pub session_id: u32,
    pub task_id: u32,
    pub result : String
}

impl TaskResult {

pub fn new(session_id: u32, task_id : u32, result: String) -> TaskResult {
    Self {
        session_id,
        task_id,
        result
    }
}

}



#[derive(Debug)]
pub struct Runner {
    pub tasks_queue: Mutex<VecDeque<Task>>,
    pub results_map: Mutex<HashMap<u64, TaskResult>>,
    pub stop: AtomicBool,
//    the_thread: std::thread::JoinHandle<i32>,
}

impl Runner {

pub fn new() -> Arc<Runner> {
    let tasks_queue = VecDeque::<Task>::new();
    let tasks_mutex = Mutex::<VecDeque::<Task>>::new(tasks_queue);

    let results_map = HashMap::<u64, TaskResult>::new();
    let results_mutex = Mutex::<HashMap::<u64, TaskResult>>::new(results_map);

    let myself = Self {
        tasks_queue : tasks_mutex,
        results_map : results_mutex,
        stop : std::sync::atomic::AtomicBool::new(false),
        //the_thread: std::thread::spawn(|| { 0 }),
    };

    //myself.run();

    Arc::new(myself)
}

pub fn run(runner: Arc<Runner>) -> std::thread::JoinHandle<i32> {
    let the_thread = thread::spawn(move || {
        loop {
            // Check if we need to stop
            if runner.stop.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }

            // Get a task from the queue
            let task_opt = {
                let mut queue = runner.tasks_queue.lock().unwrap();
                queue.pop_front()
            };
            
            if let Some(mut task) = task_opt {
                // Execute the task
                let result = task.execute();
                // Store the result
                let task_result = TaskResult::new(task.session_id, task.task_id, result);
                let mut results = runner.results_map.lock().unwrap();
                results.insert((task.session_id as u64) << 32 | (task.task_id as u64), task_result);
            } else {
                // No tasks, sleep for a while
                thread::sleep(std::time::Duration::from_millis(1));
            }
        };

        0
    });

    the_thread
}

pub fn add_task(&self, task: Task) {
    let mut queue = self.tasks_queue.lock().unwrap();
    queue.push_back(task);
}

}


#[cfg(test)]
mod tests_runner {
    use super::*;

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
            measure q[1]->c[1];".to_string();

        // Add a task to the queue
        let task = Task::new(1, 1, TaskType::Execute, qasm.clone(), "".to_string(), 5, 1, 1, "".to_string());
        runner.add_task(task);

        // Wait for the task to be processed
        std::thread::sleep(std::time::Duration::from_secs(3));
        // Check the results
        {
            let results = runner.results_map.lock().unwrap();
            assert!(results.contains_key(&(1u64 << 32 | 1u64)), "Result not found for task");
            let result = results.get(&(1u64 << 32 | 1u64)).unwrap();
            assert!(!result.result.is_empty(), "Result is empty");
        }
        // Stop the runner
        runner.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        handle.join().unwrap();
    }
}


