//! Runs each Maestro task in a fresh copy of this executable.
//!
//! The daemon owns the child and can terminate it while this module is blocked
//! sending a task, receiving a result, or waiting for native cleanup to finish.
//! The private full-duplex socket is passed as stdin; stdout/stderr remain logs.

mod protocol;

use std::io;
use std::net::Shutdown;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::runner::Task;

pub const WORKER_ARGUMENT: &str = "--worker";

#[derive(Debug)]
pub enum Outcome {
    Completed(String),
    Cancelled,
    Failed(io::Error),
}

#[cfg(test)]
#[derive(Clone, Copy)]
enum Phase {
    WritingTask = 1,
    ReadingResult = 2,
    WaitingForExit = 3,
}

#[derive(Debug, Default)]
struct Process {
    child: Option<Child>,
    socket: Option<UnixStream>,
}

/// One execution, including the period before its child has been spawned.
#[derive(Debug, Default)]
pub struct Execution {
    cancelled: AtomicBool,
    process: Mutex<Process>,
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

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Returns only after the child has exited and has been reaped. A request
    /// arriving before spawn is latched, preventing the child from starting.
    pub fn cancel_and_wait(&self) -> io::Result<()> {
        self.request_cancel();
        terminate(&mut self.process.lock().unwrap())
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
        if let Err(error) = terminate(process) {
            eprintln!("Failed to clean up Maestro worker: {error}");
        }
    }
}

fn terminate(process: &mut Process) -> io::Result<()> {
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

fn execute_with_command(task: &Task, execution: &Execution, mut command: Command) -> Outcome {
    let result = (|| {
        let (mut socket, child_socket) = UnixStream::pair()?;
        command
            .stdin(Stdio::from(OwnedFd::from(child_socket)))
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .process_group(0);
        {
            let mut process = execution.process.lock().unwrap();
            if execution.is_cancelled() {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "Task cancelled before spawn",
                ));
            }
            process.socket = Some(socket.try_clone()?);
            process.child = Some(command.spawn()?);
        }
        // Command retains its configured stdin after spawn. Close that copy of
        // the child's socket now, or a worker crash/truncated reply cannot yield EOF.
        drop(command);

        // These blocking operations hold no process/runner/session mutex.
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
            return Err(io::Error::other(format!(
                "Maestro worker exited with {status}"
            )));
        }
        Ok(result)
    })();

    // Protocol failures must also stop and reap the worker (it may still be
    // running). Successful executions have already been reaped by wait().
    let cleanup = terminate(&mut execution.process.lock().unwrap());
    if let Err(error) = cleanup {
        return Outcome::Failed(error);
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

fn worker_socket() -> io::Result<UnixStream> {
    Ok(UnixStream::from(io::stdin().as_fd().try_clone_to_owned()?))
}

/// Called by main before installing daemon signal handlers or starting threads.
pub fn run() -> io::Result<()> {
    let mut socket = worker_socket()?;
    let mut task = protocol::read_task(&mut socket)?;
    let result = task.execute();
    if result.is_empty() {
        return Err(io::Error::other("Maestro returned an empty result"));
    }
    protocol::write_result(&mut socket, &task, &result)
}

#[cfg(test)]
pub(crate) mod tests;
