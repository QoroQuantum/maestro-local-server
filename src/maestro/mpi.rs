//! Administrator-owned native MPI launch profiles. Requests select a profile
//! and rank count; they cannot supply commands, shell text, or environments.
use super::diagnostics;
use std::collections::BTreeMap;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

const RESULT_PREFIX: &str = "MAESTRO_RESULT_V2 ";
const STEP_PREFIX: &str = "MAESTRO_STEP_V2 ";
const MAX_RESULT: u64 = 64 * 1024 * 1024;
pub(super) const CLEANUP_TIMEOUT: Duration = Duration::from_secs(20);
const OUTPUT_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

// Input closure can accompany a structured native failure. Protocol and
// cancellation errors must remain authoritative even when that happens first.
fn record_failure(stop: &mut Option<io::Error>, error: io::Error) {
    if stop
        .as_ref()
        .is_none_or(|old| old.kind() == io::ErrorKind::BrokenPipe)
    {
        *stop = Some(error);
    }
}
fn native_failure(result: &str) -> bool {
    json::parse(result).is_ok_and(|value| {
        value["ok"].as_bool() == Some(false)
            && value["error"]["code"].is_string()
            && value["error"]["message"].is_string()
    })
}
fn protocol_tail(line: &[u8]) -> bool {
    [RESULT_PREFIX.as_bytes(), STEP_PREFIX.as_bytes()]
        .iter()
        .any(|prefix| {
            line.starts_with(prefix) || (line.starts_with(b"MAESTRO_") && prefix.starts_with(line))
        })
}

#[derive(Debug, Clone)]
struct Profile {
    name: String,
    kind: String,
    launcher: String,
    worker: String,
    args: Vec<String>,
    environment: BTreeMap<String, String>,
    max_ranks: u32,
    scancel: String,
}
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[derive(Debug)]
pub struct ValidationError {
    pub code: &'static str,
    message: String,
}
impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}
impl ValidationError {
    fn config(error: impl std::fmt::Display) -> Self {
        Self {
            code: "invalid_config",
            message: error.to_string(),
        }
    }
}
fn executable(path: &str) -> bool {
    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

/// Launch is server routing metadata, never a native computational field.
pub fn computational_request(request: &json::JsonValue) -> String {
    let mut native = request.clone();
    native.remove("launch");
    native.dump()
}
fn parse_profiles(value: &json::JsonValue) -> io::Result<BTreeMap<String, Profile>> {
    if value["version"].as_u32() != Some(1) || !value["profiles"].is_object() {
        return Err(invalid(
            "MPI profile file requires version=1 and a profiles object",
        ));
    }
    let mut profiles = BTreeMap::new();
    for (name, definition) in value["profiles"].entries() {
        if !definition.is_object()
            || definition.entries().any(|(key, _)| {
                ![
                    "kind",
                    "launcher",
                    "worker",
                    "args",
                    "environment",
                    "max_ranks",
                    "scancel",
                ]
                .contains(&key)
            })
        {
            return Err(invalid(format!("Invalid MPI profile: {name}")));
        }
        let text = |key: &str| {
            definition[key]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| invalid(format!("Missing {key} in MPI profile {name}")))
        };
        let kind = text("kind")?;
        if kind != "mpirun" && kind != "srun" {
            return Err(invalid("MPI kind must be mpirun or srun"));
        }
        let launcher = text("launcher")?;
        let worker = text("worker")?;
        if !Path::new(&launcher).is_absolute() || !Path::new(&worker).is_absolute() {
            return Err(invalid("MPI launcher and worker must be absolute paths"));
        }
        let max_ranks = if definition["max_ranks"].is_null() {
            32
        } else {
            definition["max_ranks"]
                .as_u32()
                .ok_or_else(|| invalid("MPI max_ranks must be an integer"))?
        };
        if max_ranks == 0 || max_ranks > 32 {
            return Err(invalid("MPI max_ranks must be in 1..32"));
        }
        let mut args = Vec::new();
        if !definition["args"].is_null() {
            if !definition["args"].is_array() {
                return Err(invalid("MPI args must be an array"));
            }
            for item in definition["args"].members() {
                args.push(
                    item.as_str()
                        .ok_or_else(|| invalid("MPI arguments must be strings"))?
                        .to_owned(),
                );
            }
        }
        let mut environment = BTreeMap::new();
        if !definition["environment"].is_null() {
            if !definition["environment"].is_object() {
                return Err(invalid("MPI environment must be an object"));
            }
            for (key, value) in definition["environment"].entries() {
                environment.insert(
                    key.to_owned(),
                    value
                        .as_str()
                        .ok_or_else(|| invalid("MPI environment values must be strings"))?
                        .to_owned(),
                );
            }
        }
        let scancel = definition["scancel"]
            .as_str()
            .unwrap_or("/usr/bin/scancel")
            .to_owned();
        if kind == "srun" && !Path::new(&scancel).is_absolute() {
            return Err(invalid("scancel must be an absolute path"));
        }
        profiles.insert(
            name.to_owned(),
            Profile {
                name: name.into(),
                kind,
                launcher,
                worker,
                args,
                environment,
                max_ranks,
                scancel,
            },
        );
    }
    Ok(profiles)
}
fn profiles() -> io::Result<BTreeMap<String, Profile>> {
    let Some(path) = std::env::var_os("MAESTRO_MPI_PROFILES") else {
        return Ok(BTreeMap::new());
    };
    let text = std::fs::read_to_string(path)?;
    let value = json::parse(&text).map_err(|error| invalid(error.to_string()))?;
    parse_profiles(&value)
}
pub fn capabilities() -> io::Result<json::JsonValue> {
    let mut values = json::JsonValue::new_array();
    for profile in profiles()?.values() {
        values.push(json::object! {
            name: profile.name.as_str(), kind: profile.kind.as_str(), max_ranks: profile.max_ranks,
            worker_present: Path::new(&profile.worker).is_file(),
            launcher_present: Path::new(&profile.launcher).is_file(),
            cancellation_available: profile.kind != "srun" || executable(&profile.scancel),
            readiness: "requires_runtime_allocation",
        }).map_err(|error| invalid(error.to_string()))?;
    }
    Ok(values)
}
fn needs_mpi(request: &json::JsonValue) -> bool {
    request["simulator"]["backend"].as_str() == Some("distributed_mpi_gpu")
        || request["requests"].members().any(needs_mpi)
}
fn selected<'a>(
    request: &json::JsonValue,
    available: &'a BTreeMap<String, Profile>,
) -> io::Result<Option<(&'a Profile, u32)>> {
    fn nested_launch(value: &json::JsonValue) -> bool {
        value["requests"]
            .members()
            .any(|item| !item["launch"].is_null() || nested_launch(item))
    }
    if nested_launch(request) {
        return Err(invalid("Specify a launch profile only on the outer task"));
    }
    let launch = &request["launch"];
    if !request.has_key("launch") {
        if needs_mpi(request) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "distributed_mpi_gpu requires a server MPI launch profile",
            ));
        }
        return Ok(None);
    }
    if !launch.is_object()
        || launch
            .entries()
            .any(|(key, _)| !["profile", "ranks"].contains(&key))
    {
        return Err(invalid("launch accepts only profile and ranks"));
    }
    let name = launch["profile"]
        .as_str()
        .ok_or_else(|| invalid("Missing MPI profile name"))?;
    let profile = available.get(name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Unknown MPI profile: {name}"),
        )
    })?;
    let ranks = launch["ranks"]
        .as_u32()
        .ok_or_else(|| invalid("Missing MPI rank count"))?;
    if ranks == 0 || !ranks.is_power_of_two() || ranks > profile.max_ranks {
        return Err(invalid(
            "MPI ranks must be a power of two within the profile limit",
        ));
    }
    Ok(Some((profile, ranks)))
}
pub fn validate(request: &json::JsonValue) -> Result<(), ValidationError> {
    validate_profiles(request, &profiles().map_err(ValidationError::config)?)
}
fn validate_profiles(
    request: &json::JsonValue,
    available: &BTreeMap<String, Profile>,
) -> Result<(), ValidationError> {
    let selected = selected(request, available).map_err(|error| ValidationError {
        code: if error.kind() == io::ErrorKind::Unsupported {
            "unsupported_capability"
        } else {
            "invalid_input"
        },
        message: error.to_string(),
    })?;
    if let Some((profile, _)) = selected {
        for (kind, path) in [("launcher", &profile.launcher), ("worker", &profile.worker)] {
            if !executable(path) {
                return Err(ValidationError::config(format!(
                    "MPI {kind} is missing or not executable: {path}"
                )));
            }
        }
        if profile.kind == "srun" && !executable(&profile.scancel) {
            return Err(ValidationError::config(format!(
                "Slurm scancel is missing or not executable: {}",
                profile.scancel
            )));
        }
    }
    Ok(())
}
fn command(profile: &Profile, ranks: u32) -> Command {
    let mut command = Command::new(&profile.launcher);
    command.args(&profile.args).envs(&profile.environment);
    if profile.kind == "mpirun" {
        command.args(["--stdin", "0", "-n", &ranks.to_string()]);
    } else {
        command.args([
            "--kill-on-bad-exit=1",
            "--input=0",
            "--ntasks",
            &ranks.to_string(),
        ]);
    }
    command.arg(&profile.worker).arg("--framed-result");
    command
}
fn valid_step(value: &str) -> bool {
    let Some((job, step)) = value.split_once('.') else {
        return false;
    };
    !job.is_empty()
        && !step.is_empty()
        && job.bytes().all(|b| b.is_ascii_digit())
        && step.bytes().all(|b| b.is_ascii_digit())
}

/// Runs inside the isolated ordinary worker. The launcher shares that worker's
/// process group. On SIGTERM it cleans up its rank group; srun additionally uses
/// the job.step identifier emitted by rank zero for explicit step cancellation.
pub struct Cancellation {
    pub cancelled: Arc<AtomicBool>,
    signals: Vec<signal_hook::SigId>,
}
impl Cancellation {
    pub fn new() -> io::Result<Self> {
        let mut guard = Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            signals: Vec::new(),
        };
        for signal in [libc::SIGTERM, libc::SIGINT] {
            guard.signals.push(signal_hook::flag::register(
                signal,
                Arc::clone(&guard.cancelled),
            )?);
        }
        Ok(guard)
    }
}
impl Drop for Cancellation {
    fn drop(&mut self) {
        for signal in self.signals.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
    }
}

pub fn run(
    request: &str,
    cancelled: &AtomicBool,
    cleanup_confirmed: &mut bool,
) -> io::Result<String> {
    let value = json::parse(request).map_err(|error| invalid(error.to_string()))?;
    let available = profiles()?;
    let (profile, ranks) =
        selected(&value, &available)?.ok_or_else(|| invalid("No MPI launch profile selected"))?;
    run_profile_tracked(
        &computational_request(&value),
        profile,
        ranks,
        cancelled,
        cleanup_confirmed,
    )
}

fn nonblocking(fd: &impl AsRawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
struct Launcher(std::process::Child);
impl Drop for Launcher {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn cancel_step(profile: &Profile, step: &str) -> io::Result<()> {
    let mut child = Launcher(
        Command::new(&profile.scancel)
            .args(["--signal=TERM", step])
            .spawn()?,
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(status) = child.0.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other("scancel rejected step cancellation"))
            };
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "scancel timed out"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[cfg(test)]
fn run_profile(
    request: &str,
    profile: &Profile,
    ranks: u32,
    cancelled: &AtomicBool,
) -> io::Result<String> {
    run_profile_tracked(request, profile, ranks, cancelled, &mut true)
}
fn run_profile_tracked(
    request: &str,
    profile: &Profile,
    ranks: u32,
    cancelled: &AtomicBool,
    cleanup_confirmed: &mut bool,
) -> io::Result<String> {
    if cancelled.load(Ordering::SeqCst) {
        return Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "Cancelled before MPI launch",
        ));
    }
    let mut child = Launcher(
        command(profile, ranks)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?,
    );
    *cleanup_confirmed = false;
    let mut stdin = child.0.stdin.take();
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing launcher stdout"))?;
    nonblocking(stdin.as_ref().unwrap())?;
    nonblocking(&stdout)?;
    let mut written = 0;
    let mut line = Vec::new();
    let mut result = None;
    let mut step = None::<String>;
    let mut stop = None::<io::Error>;
    let mut stop_time = None;
    let mut termination_sent = false;
    let mut step_cancelled = false;
    let mut exited = None;
    let mut eof = false;
    let mut cleanup_error = None;
    let mut buffer = [0u8; 8192];
    let outcome = (|| {
        loop {
            if cancelled.load(Ordering::SeqCst) {
                stop = Some(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "MPI task cancelled",
                ));
            }
            if stop.is_some() && stop_time.is_none() {
                stop_time = Some(Instant::now());
                stdin.take();
            }
            // Give an early-closing worker time to finish its error frame and exit.
            // Fatal protocol errors and explicit cancellation still terminate at once.
            if stop.as_ref().is_some_and(|error| {
                error.kind() != io::ErrorKind::BrokenPipe
                    || stop_time.unwrap().elapsed() >= OUTPUT_DRAIN_TIMEOUT
            }) && !termination_sent
                && exited.is_none()
            {
                termination_sent = true;
                unsafe {
                    libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM);
                }
            }
            if stop.is_none() {
                if let Some(input) = stdin.as_mut() {
                    match input.write(&request.as_bytes()[written..]) {
                        Ok(size) => {
                            written += size;
                            if written == request.len() {
                                stdin.take();
                            }
                        }
                        Err(error)
                            if error.kind() == io::ErrorKind::WouldBlock
                                || error.kind() == io::ErrorKind::Interrupted => {}
                        Err(error) => {
                            stop = Some(error);
                        }
                    }
                }
            }
            // No blocking reader/writer threads can outlive the launcher. Bound the
            // amount drained per poll so a log flood cannot starve cancellation.
            for _ in 0..32 {
                match stdout.read(&mut buffer) {
                    Ok(0) => {
                        eof = true;
                        break;
                    }
                    Ok(size) => {
                        for byte in &buffer[..size] {
                            if *byte == b'\n' {
                                let text = String::from_utf8_lossy(&line);
                                if let Some(value) = text.strip_prefix(RESULT_PREFIX) {
                                    if result.is_some() {
                                        record_failure(
                                            &mut stop,
                                            invalid("MPI emitted more than one external result"),
                                        );
                                    } else if json::parse(value).is_err() {
                                        record_failure(
                                            &mut stop,
                                            invalid("Invalid MPI result JSON"),
                                        );
                                    } else {
                                        result = Some(value.to_owned());
                                    }
                                } else if let Some(value) = text.strip_prefix(STEP_PREFIX) {
                                    if valid_step(value) {
                                        step = Some(value.to_owned());
                                    } else {
                                        record_failure(
                                            &mut stop,
                                            invalid("Invalid MPI Slurm step identifier"),
                                        );
                                    }
                                } else {
                                    eprintln!("{text}");
                                }
                                line.clear();
                            } else if line.len() < MAX_RESULT as usize {
                                line.push(*byte);
                            } else {
                                record_failure(&mut stop, invalid("MPI result exceeds size limit"));
                            }
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        record_failure(&mut stop, error);
                        break;
                    }
                }
            }
            // A step marker may arrive in the same poll as cancellation. Consume
            // stdout before deciding whether explicit Slurm cancellation is possible.
            if termination_sent && !step_cancelled && exited.is_none() && profile.kind == "srun" {
                if let Some(step) = step.as_deref() {
                    step_cancelled = true;
                    cleanup_error = cancel_step(profile, step).err();
                }
            }
            if exited.is_none() {
                if let Some(status) = child.0.try_wait()? {
                    exited = Some((status, Instant::now()));
                }
            }
            if let Some((status, time)) = exited {
                if let Some(error) = cleanup_error {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("Slurm cleanup could not be confirmed: {error}"),
                    ));
                }
                if stop
                    .as_ref()
                    .is_some_and(|error| error.kind() != io::ErrorKind::BrokenPipe)
                {
                    *cleanup_confirmed = true;
                    return Err(stop.take().unwrap());
                }
                if eof {
                    *cleanup_confirmed = true;
                    if !line.is_empty() {
                        if protocol_tail(&line) {
                            return Err(invalid("Truncated MPI protocol line"));
                        }
                        eprintln!("{}", String::from_utf8_lossy(&line));
                    }
                    // A fast launcher can exit between the final partial write and
                    // the next write that would have reported BrokenPipe.
                    let input_error = stop.take().or_else(|| {
                        (written < request.len()).then(|| {
                            io::Error::new(
                                io::ErrorKind::BrokenPipe,
                                "MPI launcher closed input before the complete request",
                            )
                        })
                    });
                    if let Some(error) = input_error {
                        return match result.take() {
                            Some(result) if native_failure(&result) => Ok(result),
                            _ => Err(error),
                        };
                    }
                    let result = result.take().ok_or_else(|| {
                        io::Error::other("MPI launcher returned no native result")
                    })?;
                    if !status.success()
                        && json::parse(&result).ok().and_then(|v| v["ok"].as_bool()) != Some(false)
                    {
                        return Err(io::Error::other(format!(
                            "MPI launcher exited with {status}: {result}"
                        )));
                    }
                    return Ok(result);
                }
                if time.elapsed() >= OUTPUT_DRAIN_TIMEOUT {
                    return Err(io::Error::other(
                        "MPI descendants retained stdout after launcher exit",
                    ));
                }
            }
            if stop_time.is_some_and(|time| time.elapsed() >= CLEANUP_TIMEOUT) {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "MPI launcher did not confirm rank cleanup before timeout",
                ));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    })();
    outcome.map_err(|error| {
        let native = result.as_deref().and_then(diagnostics::native_error);
        let causes: Vec<_> = native.as_ref().into_iter().chain(stop.as_ref()).collect();
        diagnostics::with_causes(
            error,
            if *cleanup_confirmed {
                "mpi_launcher_failure"
            } else {
                "cleanup_unconfirmed"
            },
            &causes,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> BTreeMap<String, Profile> {
        parse_profiles(&json::object! {version:1, profiles:{
            local:{kind:"mpirun", launcher:"/usr/bin/mpirun", worker:"/opt/maestro-mpi-worker", max_ranks:8}
        }}).unwrap()
    }
    #[test]
    fn requests_cannot_supply_commands_or_exceed_profile_allocation() {
        let profiles = fixture();
        for launch in [
            json::object! {profile:"local", ranks:3},
            json::object! {profile:"local", ranks:16},
            json::object! {profile:"missing", ranks:2},
            json::object! {profile:"local", ranks:2, command:"bad"},
        ] {
            assert!(selected(&json::object! {launch:launch}, &profiles).is_err());
        }
        assert!(
            selected(
                &json::object! {simulator:{backend:"distributed_mpi_gpu"}},
                &profiles
            )
            .is_err()
        );
        assert!(
            selected(
                &json::object! {requests:[{simulator:{backend:"distributed_mpi_gpu"}}]},
                &profiles
            )
            .is_err()
        );
        assert!(
            selected(
                &json::object! {requests:[{launch:{profile:"local", ranks:2}}]},
                &profiles
            )
            .is_err()
        );
        assert_eq!(
            selected(
                &json::object! {launch:{profile:"local", ranks:2}},
                &profiles
            )
            .unwrap()
            .unwrap()
            .1,
            2
        );
    }
    #[test]
    fn launcher_arguments_are_literal_and_include_rank_zero_stdin() {
        let profiles = fixture();
        let command = command(&profiles["local"], 2);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "--stdin",
                "0",
                "-n",
                "2",
                "/opt/maestro-mpi-worker",
                "--framed-result"
            ]
        );
        assert!(valid_step("123.4"));
        assert!(!valid_step("123.4;anything"));
    }
    fn shell(script: &str) -> Profile {
        let mut profile = fixture().remove("local").unwrap();
        profile.launcher = "/bin/sh".into();
        profile.args = vec!["-c".into(), script.into()];
        profile
    }
    #[test]
    fn launcher_result_is_unique_framed_and_not_mixed_with_logs() {
        let profile =
            shell("cat >/dev/null; echo log-message; echo 'MAESTRO_RESULT_V2 {\"ok\":true}'");
        let result = run_profile("{}", &profile, 2, &AtomicBool::new(false)).unwrap();
        assert_eq!(json::parse(&result).unwrap()["ok"].as_bool(), Some(true));
        let duplicate =
            shell("cat >/dev/null; echo 'MAESTRO_RESULT_V2 {}'; echo 'MAESTRO_RESULT_V2 {}'");
        assert!(
            run_profile("{}", &duplicate, 2, &AtomicBool::new(false))
                .unwrap_err()
                .to_string()
                .contains("more than one")
        );
    }
    #[test]
    fn malformed_missing_and_failed_mpi_results_are_errors() {
        for script in [
            "cat >/dev/null; echo 'MAESTRO_RESULT_V2 invalid'",
            "cat >/dev/null; exit 0",
            "cat >/dev/null; echo 'MAESTRO_RESULT_V2 {}'; exit 7",
        ] {
            assert!(run_profile("{}", &shell(script), 2, &AtomicBool::new(false)).is_err());
        }
    }
    #[test]
    fn cancellation_interrupts_a_launcher_that_never_reads_input() {
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let test = std::thread::spawn(move || {
            run_profile(&"x".repeat(1024 * 1024), &shell("exec sleep 30"), 2, &flag)
        });
        std::thread::sleep(Duration::from_millis(100));
        cancel.store(true, Ordering::SeqCst);
        assert_eq!(
            test.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn rank_failure_preserves_structured_native_error() {
        let profile = shell(
            "cat >/dev/null; echo 'MAESTRO_RESULT_V2 {\"ok\":false,\"error\":{\"code\":\"native_failure\"}}'; exit 1",
        );
        let value = run_profile("{}", &profile, 2, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            json::parse(&value).unwrap()["error"]["code"].as_str(),
            Some("native_failure")
        );
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    fn profile(script: &str) -> Profile {
        Profile {
            name: "probe".into(),
            kind: "mpirun".into(),
            launcher: "/bin/sh".into(),
            worker: "/bin/true".into(),
            args: vec!["-c".into(), script.into()],
            environment: BTreeMap::new(),
            max_ranks: 2,
            scancel: "/usr/bin/scancel".into(),
        }
    }
    fn run(script: &str, large: bool) -> io::Result<String> {
        run_profile(
            &"x".repeat(if large { 1024 * 1024 } else { 2 }),
            &profile(script),
            2,
            &AtomicBool::new(false),
        )
    }
    #[test]
    fn early_input_close_preserves_complete_native_failure() {
        // More than one poll's output budget: returning at launcher exit must
        // not discard the rest of a buffered structured failure.
        let value = run(r#"exec 0<&-; printf 'MAESTRO_RESULT_V2 {"ok":false,"error":{"code":"unsupported_capability","message":"'; head -c 524288 /dev/zero | tr '\000' x; printf '"}}\n'; exit 1"#, true).unwrap();
        assert_eq!(
            json::parse(&value).unwrap()["error"]["message"]
                .as_str()
                .unwrap()
                .len(),
            524288
        );
    }
    #[test]
    fn input_close_does_not_turn_success_or_malformed_frames_into_success() {
        for script in [
            r#"exec 0<&-; echo 'MAESTRO_RESULT_V2 {"ok":true}'"#,
            r#"exec 0<&-; echo 'MAESTRO_RESULT_V2 {"ok":false}'"#,
            r#"exec 0<&-; echo 'MAESTRO_RESULT_V2 broken'"#,
            r#"exec 0<&-; echo 'MAESTRO_RESULT_V2 {"ok":false,"error":{"code":"bad","message":"bad"}}'; echo 'MAESTRO_RESULT_V2 {}'"#,
        ] {
            assert!(run(script, true).is_err(), "{script}");
        }
    }
    #[test]
    fn diagnostic_tail_is_logged_but_protocol_tails_are_rejected() {
        let value = run(
            r#"cat >/dev/null; echo 'MAESTRO_RESULT_V2 {"ok":true}'; printf 'trailing diagnostic'"#,
            false,
        )
        .unwrap();
        assert_eq!(json::parse(&value).unwrap()["ok"].as_bool(), Some(true));
        assert!(
            run(
                r#"cat >/dev/null; echo 'MAESTRO_RESULT_V2 {"ok":true}'; printf M"#,
                false
            )
            .is_ok()
        );
        for tail in [
            "MAESTRO_RESULT_V2 {}",
            "MAESTRO_RES",
            "MAESTRO_STEP_V2 123.4",
        ] {
            let script = format!(
                "cat >/dev/null; echo 'MAESTRO_RESULT_V2 {{\"ok\":true}}'; printf '{tail}'"
            );
            assert!(
                run(&script, false)
                    .unwrap_err()
                    .to_string()
                    .contains("Truncated")
            );
        }
        assert!(run("cat >/dev/null; printf 'diagnostic only'", false).is_err());
    }
    #[test]
    fn cancellation_wins_over_early_input_close_and_native_result() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let worker = std::thread::spawn(move || {
            run_profile(
                &"x".repeat(1024 * 1024),
                &profile(
                    r#"exec 0<&-; echo 'MAESTRO_RESULT_V2 {"ok":false,"error":{"code":"bad","message":"bad"}}'; exec sleep 30"#,
                ),
                2,
                &flag,
            )
        });
        std::thread::sleep(Duration::from_millis(100));
        cancelled.store(true, Ordering::SeqCst);
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
    }

    #[test]
    fn failed_slurm_cleanup_cannot_be_hidden_by_a_native_error() {
        let mut launch = profile(
            r#"exec 0<&-; echo 'MAESTRO_STEP_V2 123.4'; echo 'MAESTRO_RESULT_V2 {"ok":false,"error":{"code":"bad","message":"bad"}}'; exec sleep 30"#,
        );
        launch.kind = "srun".into();
        launch.scancel = "/bin/false".into();
        // BrokenPipe itself triggers cleanup after the bounded output grace;
        // no timing-sensitive external cancellation is needed in this case.
        let error = run_profile(
            &"x".repeat(1024 * 1024),
            &launch,
            2,
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            error
                .to_string()
                .contains("Slurm cleanup could not be confirmed")
        );
        let detail = json::parse(&error.to_string()).unwrap();
        assert_eq!(detail["error"]["code"], "cleanup_unconfirmed");
        assert_eq!(detail["error"]["cleanup_confirmed"], false);
        assert_eq!(detail["error"]["causes"][0]["code"], "bad");
        assert_eq!(detail["error"]["causes"][0]["message"], "bad");
        assert_eq!(detail["error"]["causes"].len(), 2);
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    #[test]
    fn launch_validation_separates_input_capability_and_configuration() {
        let mut profiles = parse_profiles(&json::object! {version:1, profiles:{local:{kind:"srun", launcher:"/bin/true", worker:"/bin/true", scancel:"/bin/true", max_ranks:4}}}).unwrap();
        for (launch, code) in [
            (json::object! {profile:"local", ranks:3}, "invalid_input"),
            (
                json::object! {profile:"unknown", ranks:2},
                "unsupported_capability",
            ),
            (json::JsonValue::Null, "invalid_input"),
        ] {
            assert_eq!(
                validate_profiles(&json::object! {launch:launch}, &profiles)
                    .unwrap_err()
                    .code,
                code
            );
        }
        let request = json::object! {launch:{profile:"local",ranks:2}};
        assert!(validate_profiles(&request, &profiles).is_ok());
        for field in ["scancel", "launcher", "worker"] {
            let mut bad = profiles.clone();
            let profile = bad.get_mut("local").unwrap();
            match field {
                "scancel" => profile.scancel = "/missing/scancel".into(),
                "launcher" => profile.launcher = "/missing/srun".into(),
                _ => profile.worker = "/missing/worker".into(),
            }
            assert_eq!(
                validate_profiles(&request, &bad).unwrap_err().code,
                "invalid_config"
            );
        }
        profiles.get_mut("local").unwrap().scancel = "/etc/passwd".into();
        assert_eq!(
            validate_profiles(&request, &profiles).unwrap_err().code,
            "invalid_config"
        );
        let nested = json::object! {requests:[{launch:{profile:"local",ranks:2}}]};
        assert_eq!(
            validate_profiles(&nested, &profiles).unwrap_err().code,
            "invalid_input"
        );
        let native = json::parse(&computational_request(&json::object! {schema_version:2, operation:"batch", requests:[], launch:{profile:"local",ranks:2}})).unwrap();
        assert!(!native.has_key("launch"));
        assert_eq!(native["operation"], "batch");
    }
}
