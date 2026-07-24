pub mod maestro;

use signal_hook::{consts::TERM_SIGNALS, iterator::Signals};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::Duration,
};

use std::os::unix::net::{UnixListener, UnixStream};
use std::io::{Read, Write, BufReader, BufRead};
use std::error::Error;

fn server(session_manager: Arc<maestro::sessions::SessionManager>, runner: Arc<maestro::runner::Runner>, running: Arc<AtomicBool>) -> std::io::Result<()> {
    let socket_path = "/tmp/maestro.sock";

    if std::fs::metadata(socket_path).is_ok() {
        println!("A socket is already present. Deleting...");
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;
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


fn handle_connection(mut stream: UnixStream, session_manager: Arc<maestro::sessions::SessionManager>, runner: Arc<maestro::runner::Runner>) {
    thread::spawn(move || {
        stream.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
        stream.set_write_timeout(Some(Duration::from_secs(15))).unwrap();
        stream.set_nonblocking(false).unwrap();

           // Wrap the stream in a BufReader to read line by line
        let mut reader = BufReader::new(&stream);
        let mut message = String::new();
        
        // This stops instantly when it hits a '\n' byte instead of waiting for EOF
        let res = reader.read_line(&mut message);

        if let Err(e) = res {
            eprintln!("Server: Failed to read from stream: {:?}", e);
            return;
        }

        if message.is_empty() {
	        return;
        }

        stream.shutdown(std::net::Shutdown::Read).unwrap_or_else(|e| {
            eprintln!("Server: Failed to shutdown stream on read: {:?}", e);
        });

        drop(reader); // Drop the BufReader to release the borrow on stream

        println!("Server: Received message: {}\nReplying...", message);

        let res2 = stream.write_all(b"Hello!\n");
        if res2.is_err() {
            eprintln!("Server: Failed to write to stream: {:?}", res2.err());
        }

        stream.shutdown(std::net::Shutdown::Write).unwrap_or_else(|e| {
            eprintln!("Server: Failed to shutdown stream on write: {:?}", e);
        });
    });
}



fn start(wait: bool) -> (thread::JoinHandle<std::io::Result<()>>, Arc<maestro::runner::Runner>, std::thread::JoinHandle<i32>, Arc<AtomicBool>) {
    println!("Starting Maestro Local Server...");

    let running = Arc::new(AtomicBool::new(true));
    let r_signal_clone = Arc::clone(&running);
    let r_running_clone = Arc::clone(&running);

    // Setup the signal handler iterator
    let mut signals = Signals::new(TERM_SIGNALS)
        .expect("Failed to register signal listener");

    // Spawn a dedicated thread to wait for OS termination signals (SIGINT/SIGTERM)
    thread::spawn(move || {
        for signal in signals.forever() {
            println!("Received termination signal: {}", signal);
            r_signal_clone.store(false, Ordering::SeqCst);
            break; // Stop listening and allow the thread to exit
        }
    });

    let session_manager = maestro::sessions::SessionManager::new(50);
    let session_manager_clone = Arc::clone(&session_manager);
        
    // start runner as well
    let runner = maestro::runner::Runner::new();
    let runner_thread = maestro::runner::Runner::run(Arc::clone(&runner));
    let runner_clone = Arc::clone(&runner);

    let session_thread = thread::spawn(|| server(session_manager_clone, runner_clone, r_running_clone));

    // Main thread block until workers finish
    // The loop keeps the main thread alive. When the signal thread flips the flag,
    // the while loop terminates, and we move to the join phase.
    if wait {
        while running.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(200));
            session_manager.tick_sessions();
        }
    }

    (session_thread, runner, runner_thread, Arc::clone(&running))
}

fn stop(session_thread: thread::JoinHandle<std::io::Result<()>>, runner: Arc<maestro::runner::Runner>, runner_thread: std::thread::JoinHandle<i32>) {
    println!("Shutdown initiated. Waiting for worker threads to join...");

    // more cleanup here
    let _ = session_thread.join();

    // Stop the runner thread
    runner.stop();
    let _ = runner_thread.join();

    println!("Maestro Local Server shut down gracefully.");
}

fn main() {
    let (session_thread, runner, runner_thread, _) = start(true);

    stop(session_thread, runner, runner_thread);
}


#[cfg(test)]
mod server_tests {
    use super::*;
    use serial_test::serial;

#[test]
#[serial]
fn server_test() {
    let (session_thread, runner, runner_thread, stop_flag) = start(false);

    // let the server some time to start
    thread::sleep(Duration::from_millis(1500));

    // wait until the path exists
    let socket_path = "/tmp/maestro.sock";
    // loop until the file is there
    while !std::fs::metadata(socket_path).is_ok() {
        thread::sleep(Duration::from_millis(100));
    }
    thread::sleep(Duration::from_millis(100));

    // connect to the socket and do some stuff
    let stream_res = UnixStream::connect(socket_path);

    assert!(stream_res.is_ok(), "Failed to connect to server socket");
    let mut stream = stream_res.unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(15))).unwrap();
    stream.set_write_timeout(Some(Duration::from_secs(15))).unwrap();

    stream.set_nonblocking(false).unwrap();

    let res = stream.write_all(b"Test message\n");

    stream.shutdown(std::net::Shutdown::Write).unwrap_or_else(|e| {
            eprintln!("Failed to shutdown client stream on write: {:?}", e);
       });

    assert!(res.is_ok(), "Failed to write to stream");

    let mut message = String::new();

    let mut reader = BufReader::new(&stream);
    let res = reader.read_line(&mut message);
    assert!(res.is_ok(), "Failed to read from stream");

    assert!(!message.is_empty(), "Received empty message from server");

    stream.shutdown(std::net::Shutdown::Both).unwrap_or_else(|e| {
            eprintln!("Failed to shutdown client stream on read: {:?}", e);
       });

    drop(stream);

    stop_flag.store(false, Ordering::SeqCst);

    stop(session_thread, runner, runner_thread);
}

}