//use std::io::{Error, ErrorKind};
use std::collections::VecDeque; // for tasks queue
use std::collections::HashMap; // for results
use std::thread;


//use std::sync::mpsc; 
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use crate::maestro;

#[derive(Debug)]
enum TaskType {
    Execute,
    Estimate,
}

#[derive(Debug)]
struct Task {
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

    result
}

}

#[derive(Debug)]
struct TaskResult {
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
struct Runner {
    tasks_queue: std::sync::Mutex<std::collections::VecDeque<Task>>,
    results_map: std::sync::Mutex<std::collections::HashMap<u64, TaskResult>>,
    stop: std::sync::atomic::AtomicBool,
    the_thread: std::thread::JoinHandle<i32>,
}

impl Runner {

pub fn new() -> Runner {
    let tasks_queue = std::collections::VecDeque::<Task>::new();
    let tasks_mutex =  std::sync::Mutex::<std::collections::VecDeque::<Task>>::new(tasks_queue);

    let results_map = std::collections::HashMap::<u64, TaskResult>::new();
    let results_mutex = std::sync::Mutex::<std::collections::HashMap::<u64, TaskResult>>::new(results_map);

    let mut myself = Self {
        tasks_queue : tasks_mutex,
        results_map : results_mutex,
        stop : std::sync::atomic::AtomicBool::new(false),
        the_thread: std::thread::spawn(|| { 0 }),
    };

    myself.run();

    myself
}

fn run(&mut self) {
    // TODO: implement it
    self.the_thread = std::thread::spawn(|| {
        0
    });
}

}