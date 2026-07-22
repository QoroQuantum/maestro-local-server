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
use std::io::{Read, Write};

use std::net::Shutdown;
use std::io::{Error, ErrorKind};

fn server() -> std::io::Result<()> {
    let socket_path = "/tmp/maestro.sock";

    if std::fs::metadata(socket_path).is_ok() {
        println!("A socket is already present. Deleting...");
        std::fs::remove_file(socket_path)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    println!("Server is listening...");

    for stream in listener.incoming() {
	match stream {
	    Ok(socket) => {
		    let handled = handle_connection(socket);
		    match handled {
		        Ok(_) => continue,
		        Err(_) => break
            }
	    },
	    Err(_) => break
	}
    }

    println!("Listener shut down...");
    std::fs::remove_file(socket_path)?;
    
    Ok(())
}


fn handle_connection(mut stream: UnixStream) -> std::io::Result<()> {
    let mut message = String::new();
    stream.read_to_string(&mut message)?;

    if message.is_empty() {
	    return Err(Error::new(ErrorKind::Other, "Disconnect"));
    }

    println!("Received message: {}\nReplying...", message);

    stream.write(b"Hello!")?;

    Ok(())
}

fn main() {
    println!("Starting Maestro Local Server...");

    let running = Arc::new(AtomicBool::new(true));
    let r_signal_clone = Arc::clone(&running);

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


    // add something like this if using multiple threads:
    // Spawn your worker threads
    /*
    let thread_count = 4; // Adjust based on your CPU cores/workload
    let mut worker_handles = Vec::with_capacity(thread_count);

    for id in 0..thread_count {
        let r_worker_clone = Arc::clone(&running);
        
        let handle = thread::spawn(move || {
            println!("[Worker {}] Started.", id);

            // Worker loop checks the shared atomic flag on every iteration
            while r_worker_clone.load(Ordering::SeqCst) {
                // Perform real work here
                println!("[Worker {}] Processing data...", id);

                // Use a short, non-blocking sleep or break work into small chunks 
                // so the thread checks the 'running' flag frequently.
                thread::sleep(Duration::from_millis(500));
            }

            // Cleanup code for this specific worker goes here
            println!("[Worker {}] Cleaned up and exited.", id);
        });

        worker_handles.push(handle);
    }

    println!("All {} worker threads spawned. Service running...", thread_count);
    */

    let t = thread::spawn(|| server());

    // Main thread block until workers finish
    // The loop keeps the main thread alive. When the signal thread flips the flag,
    // the while loop terminates, and we move to the join phase.
    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(200));
    }

    println!("Shutdown initiated. Waiting for worker threads to join...");

    // Graceful Join
    // Iterate through all handles to ensure no threads are abruptly killed mid-task
    /*
    for (id, handle) in worker_handles.into_iter().enumerate() {
        match handle.join() {
            Ok(_) => println!("[Main] Worker {} joined successfully.", id),
            Err(_) => eprintln!("[Main] Worker {} panicked during execution.", id),
        }
    }
    */

    // more cleanup here

    let drop_stream = UnixStream::connect("/tmp/maestro.sock").unwrap();
    //let _ = drop_stream.write(b"");
    let _ = drop_stream.shutdown(Shutdown::Both);

    let _ = t.join();

    println!("Maestro Local Server shut down gracefully.");
}
