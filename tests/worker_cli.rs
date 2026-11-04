//! Exercise the production --worker entry point, including on a clean cargo test
//! build. Unit tests separately exercise cancellation using controlled workers.

use std::io::{Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn same_binary_worker_executes_and_estimates_without_starting_daemon() {
    for task_type in ["execute", "estimate"] {
        let (mut socket, child_socket) = UnixStream::pair().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut worker = Worker(
            Command::new(env!("CARGO_BIN_EXE_maestro-local-server"))
                .arg("--worker")
                .stdin(Stdio::from(OwnedFd::from(child_socket)))
                .stdout(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut ready_header = [0; 8];
        socket.read_exact(&mut ready_header).unwrap();
        let mut ready = vec![0; u64::from_be_bytes(ready_header) as usize];
        socket.read_exact(&mut ready).unwrap();
        assert_eq!(
            json::parse(std::str::from_utf8(&ready).unwrap()).unwrap()["ready"],
            true
        );
        let request = json::object! {
            version: 2, session_id: 42, task_id: 7, task_type: task_type,
            qasm: "OPENQASM 2.0;\nqreg q[1];\ncreg c[1];\nx q[0];\nmeasure q[0]->c[0];",
            observables: "Z", nr_qubits: 1, sim_type: 1, method: 1,
            options: "{\n\"shots\": 10\n}",
        }
        .dump();
        socket
            .write_all(&(request.len() as u64).to_be_bytes())
            .unwrap();
        socket.write_all(request.as_bytes()).unwrap();
        // A JSON header frame identifies the task; the raw result frame follows.
        let mut frame = || {
            let mut header = [0; 8];
            socket.read_exact(&mut header).unwrap();
            let mut payload = String::new();
            (&mut socket)
                .take(u64::from_be_bytes(header))
                .read_to_string(&mut payload)
                .unwrap();
            json::parse(&payload).unwrap()
        };
        let response = frame();
        assert_eq!(response["version"].as_u32(), Some(2));
        assert_eq!(response["session_id"].as_u32(), Some(42));
        assert_eq!(response["task_id"].as_u32(), Some(7));
        assert!(response["result"].is_null());
        let result = frame();
        if task_type == "execute" {
            assert_eq!(result["counts"]["1"].as_u32(), Some(10));
        } else {
            assert!((result["expectation_values"][0].as_f64().unwrap() + 1.0).abs() < 1e-9);
        }
        assert!(worker.0.wait().unwrap().success());
        let mut log = String::new();
        worker
            .0
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut log)
            .unwrap();
        assert!(!log.contains("Starting Maestro Local Server"));
    }
}
