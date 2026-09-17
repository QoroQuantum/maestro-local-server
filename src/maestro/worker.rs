//! Runs each Maestro task in a fresh copy of this executable.
//!
//! The daemon owns the child and can terminate it while this module is blocked
//! sending a task, receiving a result, or waiting for native cleanup to finish.
//! The private full-duplex socket is passed as stdin; stdout/stderr remain logs.

mod protocol;

use std::io::{self, Read};
use std::net::Shutdown;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::{diagnostics, runner::Task};

pub const WORKER_ARGUMENT: &str = "--worker";
// These private exit codes distinguish a failed operation from unknown remote
// cleanup. In particular, losing the result socket after cleanup is safe to reap.
pub const CLEANUP_CONFIRMED_EXIT: i32 = 76;

#[derive(Debug)]
pub struct Failure {
    pub error: io::Error,
    pub cleanup_confirmed: bool,
}

impl Failure {
    pub fn exit_code(&self) -> i32 {
        if self.cleanup_confirmed {
            CLEANUP_CONFIRMED_EXIT
        } else {
            75
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

#[derive(Debug)]
pub enum Outcome {
    Completed(String),
    Cancelled,
    Failed(io::Error),
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum Phase {
    WaitingForReady = 4,
    WritingTask = 1,
    ReadingResult = 2,
    WaitingForExit = 3,
}

#[derive(Debug, Default)]
struct Process {
    child: Option<Child>,
    socket: Option<UnixStream>,
    mpi: bool,
    ready: bool,
    // Reaping cannot turn a previously unconfirmed cleanup into a success ACK.
    cleanup_error: Option<(io::ErrorKind, String)>,
}

/// One execution, including the period before its child has been spawned.
#[derive(Debug, Default)]
pub struct Execution {
    cancelled: AtomicBool,
    process: Mutex<Process>,
    pub(super) logs: Arc<Mutex<diagnostics::CapturedOutput>>,
    stop_logs: Arc<AtomicBool>,
    log_threads: Mutex<Vec<thread::JoinHandle<()>>>,
    // Lets regression tests cancel at a specific blocking operation, rather
    // than accidentally cancelling every test before the worker has started.
    #[cfg(test)]
    phase: std::sync::atomic::AtomicU8,
}

impl Execution {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called while holding the runner's active-task lock, so cancellation and
    /// publication of a completed result have a single, well-defined ordering.
    pub fn request_cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn logs(&self) -> String {
        self.logs.lock().unwrap().text()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Returns only after the child has exited and has been reaped. A request
    /// arriving before spawn is latched, preventing the child from starting.
    pub fn cancel_and_wait(&self) -> io::Result<()> {
        self.request_cancel();
        terminate(&mut self.process.lock().unwrap())?;
        self.finish_logs();
        Ok(())
    }

    fn finish_logs(&self) {
        self.stop_logs.store(true, Ordering::SeqCst);
        // Both normal completion and cancellation wait for the same readers.
        // Never hold the process lock here: spawning registers them under it.
        for reader in self.log_threads.lock().unwrap().drain(..) {
            let _ = reader.join();
        }
    }

    fn wait(&self) -> io::Result<ExitStatus> {
        loop {
            {
                let mut process = self.process.lock().unwrap();
                let Some(child) = process.child.as_mut() else {
                    return Err(io::Error::new(
                        io::ErrorKind::Interrupted,
                        "Worker was cancelled",
                    ));
                };
                // Never hold the process lock in a blocking normal wait:
                // cancellation must work even if the child hangs during exit.
                if let Some(status) = child.try_wait()? {
                    process.child = None;
                    process.socket = None;
                    if process.mpi && !cleanup_confirmed(status) {
                        let error = unconfirmed_exit(status);
                        process.cleanup_error = Some((error.kind(), error.to_string()));
                    }
                    return Ok(status);
                }
                #[cfg(test)]
                self.phase
                    .store(Phase::WaitingForExit as u8, Ordering::SeqCst);
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Execution {
    fn drop(&mut self) {
        // Also clean up on early returns or a parent-side panic.
        let process = self
            .process
            .get_mut()
            .unwrap_or_else(|error| error.into_inner());
        if process.child.is_some()
            && let Err(error) = terminate(process)
        {
            eprintln!("Failed to clean up Maestro worker: {error}");
        }
    }
}

fn terminate(process: &mut Process) -> io::Result<()> {
    if process.child.is_none()
        && let Some((kind, message)) = &process.cleanup_error
    {
        return Err(io::Error::new(*kind, message.clone()));
    }
    let result = terminate_child(process);
    if let Err(error) = &result {
        process.cleanup_error = Some((error.kind(), error.to_string()));
    } else {
        // A transient signalling error may leave a live child. A later attempt
        // must still be allowed to stop it and observe actual cleanup.
        process.cleanup_error = None;
    }
    result
}

fn cleanup_confirmed(status: ExitStatus) -> bool {
    status.success() || status.code() == Some(CLEANUP_CONFIRMED_EXIT)
}

fn unconfirmed_exit(status: ExitStatus) -> io::Error {
    io::Error::other(format!(
        "MPI supervisor exited without confirming cleanup: {status}"
    ))
}

fn terminate_child(process: &mut Process) -> io::Result<()> {
    if let Some(socket) = &process.socket {
        // Wake any parent-side read/write, including a socket inherited by a
        // descendant of the worker. No runner/session locks are held here.
        let _ = socket.shutdown(Shutdown::Both);
    }
    if let Some(child) = process.child.as_mut() {
        // spawn() gives the worker its own process group. Signal it before
        // reaping the leader so its PID cannot be reused for an unrelated group.
        let group = libc::pid_t::try_from(child.id())
            .map_err(|_| io::Error::other("Invalid worker process ID"))?;
        if process.mpi && process.ready {
            // Let the launcher finish remote-rank cleanup before reaping its
            // supervisor. A forced timeout is an error, never a success ACK.
            unsafe {
                libc::kill(group, libc::SIGTERM);
            }
            let deadline =
                std::time::Instant::now() + super::mpi::CLEANUP_TIMEOUT + Duration::from_secs(5);
            loop {
                if let Some(status) = child.try_wait()? {
                    process.child = None;
                    process.socket = None;
                    return if cleanup_confirmed(status) {
                        Ok(())
                    } else {
                        Err(unconfirmed_exit(status))
                    };
                }
                if std::time::Instant::now() >= deadline {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            process.child = None;
            process.socket = None;
            return Err(io::Error::other(
                "MPI cleanup timed out; remote rank termination could not be confirmed",
            ));
        }
        // SAFETY: kill takes an integer process-group ID, with no pointers.
        if unsafe { libc::kill(-group, libc::SIGKILL) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        // Also address the owned child directly if native code changed its group.
        child.kill()?;
        child.wait()?;
        process.child = None;
    }
    process.socket = None;
    Ok(())
}

fn worker_command() -> io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    #[cfg(not(test))]
    command.arg(WORKER_ARGUMENT);
    // A unit-test executable uses libtest's main, not this binary's main.
    // Re-enter the worker through a dedicated test in that executable instead.
    #[cfg(test)]
    command
        .args([
            "--exact",
            "maestro::worker::tests::worker_entry",
            "--nocapture",
        ])
        .env("MAESTRO_TEST_WORKER", "1");
    Ok(command)
}

/// The runner's replacement for the former in-process `task.execute()` call.
pub fn execute(task: &Task, execution: &Execution) -> Outcome {
    match worker_command() {
        Ok(command) => execute_with_command(task, execution, command),
        Err(error) => Outcome::Failed(error),
    }
}

/// Parse untrusted native input in a child before allocating a public task ID.
/// A rejected request is a completed validation response, not a worker failure.
pub fn validate_request(request: &str) -> io::Result<String> {
    validate_with_command(request, worker_command()?)
}
fn validate_with_command(request: &str, command: Command) -> io::Result<String> {
    let task = Task::new(
        0,
        0,
        super::runner::TaskType::ValidateRequest,
        request.to_owned(),
        String::new(),
        0,
        0,
        0,
        String::new(),
    );
    let execution = Execution::new();
    match execute_with_command(&task, &execution, command) {
        Outcome::Completed(value) => Ok(value),
        Outcome::Failed(error) => Err(io::Error::other(format!(
            "Native validation worker failed: {error}; {}",
            execution.logs()
        ))),
        Outcome::Cancelled => Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Validation cancelled",
        )),
    }
}

fn execute_with_command(task: &Task, execution: &Execution, mut command: Command) -> Outcome {
    let result = (|| {
        let (mut socket, child_socket) = UnixStream::pair()?;
        command
            .stdin(Stdio::from(OwnedFd::from(child_socket)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        {
            let mut process = execution.process.lock().unwrap();
            if execution.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Task cancelled before spawn",
                ));
            }
            process.mpi = matches!(task.task_type, super::runner::TaskType::Request)
                && json::parse(&task.qasm).is_ok_and(|value| value["launch"].is_object());
            process.socket = Some(socket.try_clone()?);
            let mut child = command.spawn()?;
            let mut log_threads = execution.log_threads.lock().unwrap();
            if let Some(stdout) = child.stdout.take() {
                log_threads.push(drain_log(
                    stdout,
                    Arc::clone(&execution.logs),
                    Arc::clone(&execution.stop_logs),
                ));
            }
            if let Some(stderr) = child.stderr.take() {
                log_threads.push(drain_log(
                    stderr,
                    Arc::clone(&execution.logs),
                    Arc::clone(&execution.stop_logs),
                ));
            }
            process.child = Some(child);
        }
        // Command retains its configured stdin after spawn. Close that copy of
        // the child's socket now, or a worker crash/truncated reply cannot yield EOF.
        drop(command);

        // These blocking operations hold no process/runner/session mutex.
        #[cfg(test)]
        execution
            .phase
            .store(Phase::WaitingForReady as u8, Ordering::SeqCst);
        protocol::read_ready(&mut socket)?;
        {
            let mut process = execution.process.lock().unwrap();
            if execution.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Task cancelled before upload",
                ));
            }
            // Mark before sending any task bytes. Without them the worker
            // cannot launch ranks, so cancellation before this point is local.
            process.ready = true;
        }
        #[cfg(test)]
        execution
            .phase
            .store(Phase::WritingTask as u8, Ordering::SeqCst);
        protocol::write_task(&mut socket, task)?;
        #[cfg(test)]
        execution
            .phase
            .store(Phase::ReadingResult as u8, Ordering::SeqCst);
        let result = protocol::read_result(&mut socket, task)?;
        let status = execution.wait()?;
        if !status.success() {
            if let Some(error) = diagnostics::native_error(&result) {
                return Err(error);
            }
            return Err(io::Error::other(format!(
                "Maestro worker exited with {status}"
            )));
        }
        // Native errors still represent a finished worker. Reap it before
        // classifying the result, especially after MPI has finalized its ranks.
        if matches!(task.task_type, super::runner::TaskType::Request)
            && json::parse(&result).ok().and_then(|v| v["ok"].as_bool()) != Some(true)
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, result));
        }
        Ok(result)
    })();

    // Protocol failures must also stop and reap the worker (it may still be
    // running). Successful executions have already been reaped by wait().
    let cleanup = terminate(&mut execution.process.lock().unwrap());
    execution.finish_logs();
    if let Err(error) = cleanup {
        return Outcome::Failed(diagnostics::with_causes(
            error,
            "cleanup_unconfirmed",
            &result.as_ref().err().into_iter().collect::<Vec<_>>(),
        ));
    }
    if execution.is_cancelled() {
        Outcome::Cancelled
    } else {
        match result {
            Ok(result) => Outcome::Completed(result),
            Err(error) => Outcome::Failed(error),
        }
    }
}

// Drain pipes concurrently and keep a bounded prefix and tail. Nonblocking reads let
// cleanup finish even if a native descendant inherited a log descriptor.
fn drain_log<T: Read + AsRawFd + Send + 'static>(
    mut pipe: T,
    logs: Arc<Mutex<diagnostics::CapturedOutput>>,
    stop: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let fd = pipe.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
            return;
        }
        let mut buffer = [0u8; 8192];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    logs.lock().unwrap().append(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn worker_socket() -> io::Result<UnixStream> {
    Ok(UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?))
}

/// Called by main before installing daemon signal handlers or starting threads.
pub fn run() -> Result<(), Failure> {
    let mut cleanup_confirmed = true; // No launcher has started.
    let result = (|| {
        #[cfg(test)]
        tests::pause_worker("before_ready", None);
        let signals = super::mpi::Cancellation::new()?;
        let mut socket = worker_socket()?;
        protocol::write_ready(&mut socket)?;
        #[cfg(test)]
        tests::pause_worker("before_read", Some(&signals.cancelled));
        let mut task = protocol::read_task(&mut socket)?;
        #[cfg(test)]
        tests::pause_worker("after_task", Some(&signals.cancelled));
        if signals.cancelled.load(Ordering::SeqCst) {
            return Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "Cancelled before launch",
            ));
        }
        let result = if matches!(task.task_type, super::runner::TaskType::Request)
            && json::parse(&task.qasm).is_ok_and(|value| value["launch"].is_object())
        {
            match super::mpi::run(&task.qasm, &signals.cancelled, &mut cleanup_confirmed) {
                Ok(result) => result,
                Err(error) if !cleanup_confirmed => {
                    let error = diagnostics::with_causes(error, "cleanup_unconfirmed", &[]);
                    // Publish diagnostics even when cleanup is uncertain. The
                    // nonzero exit still prevents successful task/cancel ACKs.
                    let _ =
                        protocol::write_result(&mut socket, &task, &diagnostics::envelope(&error));
                    return Err(error);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => return Err(error),
                Err(error) => diagnostics::envelope(&diagnostics::with_causes(
                    error,
                    "mpi_launcher_failure",
                    &[],
                )),
            }
        } else {
            task.execute()
        };
        #[cfg(test)]
        tests::pause_worker("after_cleanup", Some(&signals.cancelled));
        if result.is_empty() {
            return Err(io::Error::other("Maestro returned an empty result"));
        }
        protocol::write_result(&mut socket, &task, &result)
    })();
    result.map_err(|error| Failure {
        error,
        cleanup_confirmed,
    })
}

#[cfg(test)]
pub(crate) mod tests;
