pub mod maestro;

use signal_hook::{consts::TERM_SIGNALS, iterator::Signals};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};

mod maestro_server {
    use super::*;

    fn server(
        session_manager: Arc<maestro::sessions::SessionManager>,
        runner: Arc<maestro::runner::Runner>,
        running: Arc<AtomicBool>,
        use_tmp_file: bool,
    ) -> std::io::Result<()> {
        println!("Creating Maestro Socket...");
        let socket_path = if use_tmp_file {
            "/tmp/maestro.sock"
        } else {
            "/run/maestro.sock"
        };

        if std::fs::metadata(socket_path).is_ok() {
            println!("A socket is already present. Deleting...");
            std::fs::remove_file(socket_path)?;
        }

        println!("Now really onto listening...");

        let listener = UnixListener::bind(socket_path)?;

        println!("Granting read/write permissions to the socket file...");

        // Grant read/write permissions to the group
        let mut perms = std::fs::metadata(socket_path)?.permissions();
        perms.set_mode(0o660); // 0o660 = rw-rw----
        std::fs::set_permissions(socket_path, perms)?;

        listener.set_nonblocking(true)?; // So accept() won't block forever
        println!("Server is listening...");

        while running.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _addr)) => {
                    handle_connection(stream, Arc::clone(&session_manager), Arc::clone(&runner));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No connection yet, sleep a bit
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    eprintln!("Accept error: {}", e);
                    break;
                }
            }
        }

        println!("Listener shut down...");

        // Explicitly drop the listener to close the socket
        drop(listener);
        std::fs::remove_file(socket_path)?;

        Ok(())
    }

    // Valid received commands:
    // SESSION CREATE
    // SESSION <session_id> EXISTS
    // SESSION <session_id> DELETE
    fn handle_connection(
        mut stream: UnixStream,
        session_manager: Arc<maestro::sessions::SessionManager>,
        runner: Arc<maestro::runner::Runner>,
    ) {
        thread::spawn(move || {
            stream
                .set_read_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(15)))
                .unwrap();
            stream.set_nonblocking(false).unwrap();

            let mut message = String::new();

            loop {
                message.clear();
                let mut reader = BufReader::new(&stream);

                let res = reader.read_line(&mut message);
                if res.is_err() {
                    break;
                }

                if message.is_empty() {
                    return;
                }

                let response;

                if let Some(substring) = message.strip_prefix("SESSION ") {
                    response =
                        session_manager.handle_request(substring.to_string(), Arc::clone(&runner));
                } else {
                    response = String::from("ERROR Unknown command\n");
                }

                //stream.shutdown(std::net::Shutdown::Read).unwrap_or_else(|e| {
                //    eprintln!("Server: Failed to shutdown stream on read: {:?}", e);
                //});

                //drop(reader); // Drop the BufReader to release the borrow on stream

                let res2 = stream.write_all(response.as_bytes());
                if res2.is_err() {
                    break;
                }

                //stream.shutdown(std::net::Shutdown::Write).unwrap_or_else(|e| {
                //    eprintln!("Server: Failed to shutdown stream on write: {:?}", e);
                //});
            }
        });
    }

    pub fn start(
        wait: bool,
        use_tmp_file: bool,
    ) -> (
        thread::JoinHandle<std::io::Result<()>>,
        Arc<maestro::runner::Runner>,
        std::thread::JoinHandle<i32>,
        Arc<AtomicBool>,
    ) {
        println!("Starting Maestro Local Server...");

        let running = Arc::new(AtomicBool::new(true));
        let r_signal_clone = Arc::clone(&running);
        let r_running_clone = Arc::clone(&running);

        // Setup the signal handler iterator
        let mut signals = Signals::new(TERM_SIGNALS).expect("Failed to register signal listener");

        // Spawn a dedicated thread to wait for OS termination signals (SIGINT/SIGTERM)
        thread::spawn(move || {
            if let Some(signal) = signals.forever().next() {
                println!("Received termination signal: {}", signal);
                r_signal_clone.store(false, Ordering::SeqCst);
                // Stop listening and allow the thread to exit
            }
        });

        let session_manager = maestro::sessions::SessionManager::new(50);
        let session_manager_clone = Arc::clone(&session_manager);

        // start runner as well
        println!("Starting Maestro Runner...");

        let runner = maestro::runner::Runner::new();
        let runner_thread = maestro::runner::Runner::run(Arc::clone(&runner));
        let runner_clone = Arc::clone(&runner);

        println!("Starting Maestro Server...");
        let session_thread = thread::spawn(move || {
            server(
                session_manager_clone,
                runner_clone,
                r_running_clone,
                use_tmp_file,
            )
        });

        // Main thread block until workers finish
        // The loop keeps the main thread alive. When the signal thread flips the flag,
        // the while loop terminates, and we move to the join phase.
        if wait {
            println!("Main thread waiting for termination signal...");

            while running.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(200));
                session_manager.tick_sessions(Arc::clone(&runner));
            }
        }

        (session_thread, runner, runner_thread, Arc::clone(&running))
    }

    pub fn stop(
        session_thread: thread::JoinHandle<std::io::Result<()>>,
        runner: Arc<maestro::runner::Runner>,
        runner_thread: std::thread::JoinHandle<i32>,
    ) {
        println!("Shutdown initiated. Waiting for worker threads to join...");

        // more cleanup here
        let _ = session_thread.join();

        // Stop the runner thread
        runner.stop();
        let _ = runner_thread.join();

        println!("Maestro Local Server shut down gracefully.");
    }
}

fn main() {
    let (session_thread, runner, runner_thread, _) = maestro_server::start(true, false);

    maestro_server::stop(session_thread, runner, runner_thread);
}

#[cfg(test)]
mod server_tests {
    use super::*;
    use serial_test::serial;

    fn send_command_noclose(mut stream: &mut UnixStream, command: &str) -> std::io::Result<String> {
        stream.write_all(command.as_bytes())?;
        let mut response = String::new();
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut response)?;
        Ok(response)
    }

    fn send_command_close(command: &str) -> std::io::Result<String> {
        let socket_path = "/tmp/maestro.sock";
        let mut stream = UnixStream::connect(socket_path)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream.set_nonblocking(false).unwrap();
        stream.write_all(command.as_bytes())?;
        // can close the write end now
        stream
            .shutdown(std::net::Shutdown::Write)
            .unwrap_or_else(|e| {
                eprintln!("Failed to shutdown client stream on write: {:?}", e);
            });

        let mut response = String::new();
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut response)?;

        stream
            .shutdown(std::net::Shutdown::Read)
            .unwrap_or_else(|e| {
                eprintln!("Failed to shutdown client stream on read: {:?}", e);
            });
        Ok(response)
    }

    #[test]
    #[serial]
    fn server_test_nodisconnect() {
        let (session_thread, runner, runner_thread, stop_flag) = maestro_server::start(false, true);

        // let the server some time to start
        thread::sleep(Duration::from_millis(1500));

        // wait until the path exists
        let socket_path = "/tmp/maestro.sock";
        // loop until the file is there

        //assert!(std::fs::metadata(socket_path).is_ok(), "Socket file does not exist after waiting for server to start");

        while !std::fs::metadata(socket_path).is_ok() {
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(100));

        // connect to the socket and do some stuff
        let stream_res = UnixStream::connect(socket_path);

        assert!(stream_res.is_ok(), "Failed to connect to server socket");
        let mut stream = stream_res.unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(15)))
            .unwrap();

        stream.set_nonblocking(false).unwrap();

        // SESSION stuff
        let response = send_command_noclose(&mut stream, "SESSION 0 EXISTS\n").unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent session, got: {}",
            response
        );

        let response = send_command_noclose(&mut stream, "SESSION CREATE\n").unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for session creation, got: {}",
            response
        );

        let session_id = response.trim_start_matches("OK ").trim();
        assert!(
            session_id == "0",
            "Expected session ID 0, got: {}",
            session_id
        );

        let command = format!("SESSION {} EXISTS\n", session_id);
        let response = send_command_noclose(&mut stream, &command).unwrap();
        assert!(
            response == "OK YES\n",
            "Expected OK YES for existing session, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!(
                "SESSION {} EXISTS\n",
                session_id.parse::<i32>().unwrap() + 1
            ),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non existing session, got: {}",
            response
        );

        let command = format!("SESSION {} DELETE\n", session_id);
        let response = send_command_noclose(&mut stream, &command).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for deleting existing session, got: {}",
            response
        );

        let command = format!("SESSION {} EXISTS\n", session_id);
        let response = send_command_noclose(&mut stream, &command).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent session, got: {}",
            response
        );

        // now create one to be used later

        let response = send_command_noclose(&mut stream, "SESSION CREATE\n").unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for session creation, got: {}",
            response
        );

        let expected_id = session_id.parse::<i32>().unwrap() + 1;
        let session_id = response.trim_start_matches("OK ").trim();
        assert!(
            session_id == expected_id.to_string(),
            "Expected session ID {}, got: {}",
            expected_id,
            session_id
        );

        // TASK stuff
        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 EXISTS\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK CREATE\n", session_id),
        )
        .unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for task creation, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 EXISTS\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK YES\n",
            "Expected OK YES for existing task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 VALID\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not valid task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 PENDING\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not pending task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 RUNNING\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not running task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 FINISHED\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not finished task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 EXECUTE\n", session_id),
        )
        .unwrap();
        assert!(
            response == "ERROR Task is not valid or does not exist\n",
            "Expected ERROR Task is not valid or does not exist for existing but not valid task, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 GET_RESULTS\n", session_id),
        )
        .unwrap();
        assert!(
            response == "ERROR Result not available\n",
            "Expected ERROR Result not available for existing but not finished task, got: {}",
            response
        );

        // now let's make the task valid by setting its values
        // set type to EXECUTE
        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 SET_TYPE EXECUTE\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting task type, got: {}",
            response
        );

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 SET_QUBITS 5\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting number of qubits, got: {}",
            response
        );

        let response = send_command_noclose(&mut stream, &format!("SESSION {} TASK 0 SET_QASM OPENQASM 2.0; creg c[3]; qreg q[3]; x q[0]; cx q[0],q[1]; measure q[0]->c[0]; measure q[1]->c[1];\n", session_id)).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting qasm string, got: {}",
            response
        );

        // now it should be valid
        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 VALID\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK YES\n",
            "Expected OK YES for existing and valid task, got: {}",
            response
        );

        // execute it
        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 EXECUTE\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for executing valid task, got: {}",
            response
        );

        // now wait until it's finished
        let mut finished = false;
        while !finished {
            let response = send_command_noclose(
                &mut stream,
                &format!("SESSION {} TASK 0 FINISHED\n", session_id),
            )
            .unwrap();
            if response == "OK YES\n" {
                finished = true;
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }

        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 GET_RESULTS\n", session_id),
        )
        .unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for getting results of finished task, got: {}",
            response
        );

        let json_result = response.trim_start_matches("OK ").trim();

        let parsed = json::parse(json_result).unwrap();

        assert!(
            parsed.has_key("counts"),
            "Execute result does not have 'counts' key"
        );
        assert!(
            parsed.has_key("time_taken"),
            "Execute result does not have 'time_taken' key"
        );
        assert!(
            parsed.has_key("simulator"),
            "Execute result does not have 'simulator' key"
        );
        assert!(
            parsed.has_key("method"),
            "Execute result does not have 'method' key"
        );

        assert!(
            parsed["counts"].has_key("11000"),
            "Execute result counts does not have '11000' key"
        );
        assert!(
            parsed["counts"]["11000"].as_i32().unwrap() == 1,
            "Execute result counts '11000' value is not 1"
        );
        assert!(
            parsed["simulator"] == "aer",
            "Execute result simulator is not 'aer'"
        );
        assert!(
            parsed["method"] == "statevector",
            "Execute result method is not 'statevector'"
        );

        // should not exist anymore
        let response = send_command_noclose(
            &mut stream,
            &format!("SESSION {} TASK 0 EXISTS\n", session_id),
        )
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent task, got: {}",
            response
        );

        stream
            .shutdown(std::net::Shutdown::Both)
            .unwrap_or_else(|e| {
                eprintln!("Failed to shutdown client stream on read: {:?}", e);
            });

        drop(stream);

        stop_flag.store(false, Ordering::SeqCst);

        maestro_server::stop(session_thread, runner, runner_thread);
    }

    #[test]
    #[serial]
    fn server_test_disconnect() {
        let (session_thread, runner, runner_thread, stop_flag) = maestro_server::start(false, true);

        // let the server some time to start
        thread::sleep(Duration::from_millis(1500));

        // wait until the path exists
        let socket_path = "/tmp/maestro.sock";
        // loop until the file is there
        while !std::fs::metadata(socket_path).is_ok() {
            thread::sleep(Duration::from_millis(100));
        }
        thread::sleep(Duration::from_millis(100));

        // SESSION stuff
        let response = send_command_close("SESSION 0 EXISTS").unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent session, got: {}",
            response
        );

        let response = send_command_close("SESSION CREATE").unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for session creation, got: {}",
            response
        );

        let session_id = response.trim_start_matches("OK ").trim();
        assert!(
            session_id == "0",
            "Expected session ID 0, got: {}",
            session_id
        );

        let response = send_command_close(&format!("SESSION {} EXISTS", session_id)).unwrap();
        assert!(
            response == "OK YES\n",
            "Expected OK YES for existing session, got: {}",
            response
        );

        let response = send_command_close(&format!(
            "SESSION {} EXISTS",
            session_id.parse::<i32>().unwrap() + 1
        ))
        .unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non existing session, got: {}",
            response
        );

        let response = send_command_close(&format!("SESSION {} DELETE", session_id)).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for deleting existing session, got: {}",
            response
        );

        let response = send_command_close(&format!("SESSION {} EXISTS", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent session, got: {}",
            response
        );

        // now create one to be used later

        let response = send_command_close("SESSION CREATE").unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for session creation, got: {}",
            response
        );

        let expected_id = session_id.parse::<i32>().unwrap() + 1;
        let session_id = response.trim_start_matches("OK ").trim();
        assert!(
            session_id == expected_id.to_string(),
            "Expected session ID {}, got: {}",
            expected_id,
            session_id
        );

        // TASK stuff
        let response =
            send_command_close(&format!("SESSION {} TASK 0 EXISTS", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent task, got: {}",
            response
        );

        let response = send_command_close(&format!("SESSION {} TASK CREATE", session_id)).unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for task creation, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 EXISTS", session_id)).unwrap();
        assert!(
            response == "OK YES\n",
            "Expected OK YES for existing task, got: {}",
            response
        );

        let response = send_command_close(&format!("SESSION {} TASK 0 VALID", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not valid task, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 PENDING", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not pending task, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 RUNNING", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not running task, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 FINISHED", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for existing but not finished task, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 EXECUTE", session_id)).unwrap();
        assert!(
            response == "ERROR Task is not valid or does not exist\n",
            "Expected ERROR Task is not valid or does not exist for existing but not valid task, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 GET_RESULTS", session_id)).unwrap();
        assert!(
            response == "ERROR Result not available\n",
            "Expected ERROR Result not available for existing but not finished task, got: {}",
            response
        );

        // now let's make the task valid by setting its values
        // set type to ESTIMATE
        let response =
            send_command_close(&format!("SESSION {} TASK 0 SET_TYPE ESTIMATE", session_id))
                .unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting task type, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 SET_QUBITS 5", session_id)).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting number of qubits, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 SET_SIM_TYPE 1", session_id)).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting simulator type, got: {}",
            response
        );

        let response =
            send_command_close(&format!("SESSION {} TASK 0 SET_METHOD 1", session_id)).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting method, got: {}",
            response
        );

        let response = send_command_close(&format!(
            "SESSION {} TASK 0 SET_QASM OPENQASM 2.0; creg c[3]; qreg q[3]; x q[0]; cx q[0],q[1];",
            session_id
        ))
        .unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting qasm string, got: {}",
            response
        );

        let response = send_command_close(&format!(
            "SESSION {} TASK 0 SET_OBSERVABLES zzz;yyy;xxx",
            session_id
        ))
        .unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for setting observables, got: {}",
            response
        );

        // now it should be valid
        let response = send_command_close(&format!("SESSION {} TASK 0 VALID", session_id)).unwrap();
        assert!(
            response == "OK YES\n",
            "Expected OK YES for existing and valid task, got: {}",
            response
        );

        // execute it
        let response =
            send_command_close(&format!("SESSION {} TASK 0 EXECUTE", session_id)).unwrap();
        assert!(
            response == "OK\n",
            "Expected OK for executing valid task, got: {}",
            response
        );

        // now wait until it's finished
        let mut finished = false;
        while !finished {
            let response =
                send_command_close(&format!("SESSION {} TASK 0 FINISHED", session_id)).unwrap();
            if response == "OK YES\n" {
                finished = true;
            } else {
                thread::sleep(Duration::from_millis(10));
            }
        }

        let response =
            send_command_close(&format!("SESSION {} TASK 0 GET_RESULTS", session_id)).unwrap();
        assert!(
            response.starts_with("OK "),
            "Expected OK for getting results of finished task, got: {}",
            response
        );

        let json_result = response.trim_start_matches("OK ").trim();

        let parsed = json::parse(json_result).unwrap();

        assert!(
            parsed.has_key("expectation_values"),
            "Estimate result does not have 'expectation_values' key"
        );
        assert!(
            parsed.has_key("time_taken"),
            "Estimate result does not have 'time_taken' key"
        );
        assert!(
            parsed.has_key("simulator"),
            "Estimate result does not have 'simulator' key"
        );
        assert!(
            parsed.has_key("method"),
            "Estimate result does not have 'method' key"
        );

        assert!(
            parsed["expectation_values"].is_array(),
            "Estimate result expectation_values is not an array"
        );

        // expectation_values is a list of floats, check if the result is meaningful!
        let values = &parsed["expectation_values"];

        let values_cnt = values.len();
        assert!(
            values_cnt == 3,
            "Estimate result expectation_values is not 3"
        );

        for (i, value) in values.members().enumerate() {
            assert!(
                value.is_number(),
                "Estimate result expectation_values contains non-number value"
            );

            if i == 0 {
                assert!(
                    value.as_f64().unwrap() >= 0.99999 && value.as_f64().unwrap() <= 1.00001,
                    "Estimate result expectation_values contains value out of range [0.99999, 1.00001], Value: {}",
                    value.as_f64().unwrap()
                );
            } else {
                assert!(
                    value.as_f64().unwrap() >= -0.00001 && value.as_f64().unwrap() <= 0.00001,
                    "Estimate result expectation_values contains value out of range [-0.00001, 0.00001], Value: {}",
                    value.as_f64().unwrap()
                );
            }
        }

        assert!(
            !values.is_empty(),
            "Estimate result expectation_values is empty"
        );

        assert!(
            parsed["simulator"] == "qcsim",
            "Estimate result simulator is not 'qcsim'"
        );
        assert!(
            parsed["method"] == "matrix_product_state",
            "Estimate result method is not 'matrix_product_state'"
        );

        // should not exist anymore
        let response =
            send_command_close(&format!("SESSION {} TASK 0 EXISTS", session_id)).unwrap();
        assert!(
            response == "OK NO\n",
            "Expected OK NO for non-existent task, got: {}",
            response
        );

        stop_flag.store(false, Ordering::SeqCst);

        maestro_server::stop(session_thread, runner, runner_thread);
    }
}
