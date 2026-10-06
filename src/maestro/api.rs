//! Versioned, lossless public JSON API alongside the legacy command protocol.
use super::{mpi, runner::Runner, sessions::SessionManager, simulators, worker};
use std::sync::Arc;

pub const MAX_REQUEST_BYTES: usize = 16 * 1024 * 1024;
pub const VERSION: u32 = 2;

pub fn session_lease_seconds() -> u32 {
    std::env::var("MAESTRO_SESSION_LEASE_SECONDS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0)
        .unwrap_or(3600)
}

fn failure(code: &str, message: &str) -> json::JsonValue {
    json::object! { version: VERSION, ok: false, error: { code: code, message: message } }
}

pub fn oversized_response() -> String {
    format!(
        "OK {}\n",
        failure("invalid_input", "Request exceeds size limit").dump()
    )
}

fn validate_envelope(response: &str) -> Result<(), json::JsonValue> {
    let malformed = || failure("native_failure", "Invalid native validation response");
    let value = json::parse(response).map_err(|_| malformed())?;
    if !value.is_object() || value["schema_version"].as_u32() != Some(VERSION) {
        return Err(malformed());
    }
    match value["ok"].as_bool() {
        Some(true) => Ok(()),
        Some(false)
            if value["error"]["code"].is_string() && value["error"]["message"].is_string() =>
        {
            Err(failure(
                value["error"]["code"].as_str().unwrap(),
                value["error"]["message"].as_str().unwrap(),
            ))
        }
        _ => Err(malformed()),
    }
}

fn dispatch(
    document: &str,
    manager: &Arc<SessionManager>,
    runner: &Arc<Runner>,
) -> json::JsonValue {
    if document.len() > MAX_REQUEST_BYTES {
        return failure("invalid_input", "Request exceeds size limit");
    }
    let value = match json::parse(document) {
        Ok(value) if value.is_object() => value,
        _ => return failure("invalid_input", "Expected a JSON object"),
    };
    if value["version"].as_u32() != Some(VERSION) {
        return failure(
            "unsupported_version",
            "This server supports public API version 2",
        );
    }
    let Some(command) = value["command"].as_str() else {
        return failure("invalid_input", "Missing command");
    };
    let allowed = match command {
        "capabilities" | "status" => &["version", "command"][..],
        "submit" => &["version", "command", "session_id", "request"][..],
        "keepalive" => &["version", "command", "session_id"][..],
        "logs" => &["version", "command", "session_id", "task_id"][..],
        _ => return failure("unsupported_operation", "Unknown API command"),
    };
    if value.entries().any(|(key, _)| !allowed.contains(&key)) {
        return failure("invalid_input", "Unknown API field");
    }
    if command == "status" {
        return json::object! {version: VERSION, ok: true, status: runner.status()};
    }
    if command == "capabilities" {
        let native = match json::parse(&simulators::capabilities()) {
            Ok(native) if native["ok"].as_bool() == Some(true) => native,
            _ => return failure("native_failure", "Native capability query failed"),
        };
        let profiles = match mpi::capabilities() {
            Ok(value) => value,
            Err(error) => return failure("invalid_config", &error.to_string()),
        };
        return json::object! {
            version: VERSION, ok: true, native: native,
            server: {
                commands: ["capabilities", "submit", "logs", "keepalive", "status"],
                build: {version: env!("CARGO_PKG_VERSION"), source_revision: env!("MAESTRO_SERVER_REVISION")},
                session_lease_seconds: session_lease_seconds(),
                max_request_bytes: MAX_REQUEST_BYTES,
                execution: "native_worker_process",
                mpi_profiles: profiles,
            }
        };
    }
    let Some(id) = value["session_id"].as_u32() else {
        return failure("invalid_input", "Invalid session_id");
    };
    let Some(session) = manager.get_session(id) else {
        return failure("session_not_found", "Session does not exist");
    };
    session.reset_timer();
    if command == "keepalive" {
        return json::object! { version: VERSION, ok: true };
    }
    if command == "submit" {
        if !value["request"].is_object() {
            return failure("invalid_input", "Missing native request object");
        }
        let request = value["request"].dump();
        if let Err(error) = mpi::validate(&value["request"]) {
            return failure(error.code, &error.to_string());
        }
        // Native parser crashes cannot terminate the daemon. No public task is
        // allocated until this isolated validation and launch validation succeed.
        let response =
            match worker::validate_request(&mpi::computational_request(&value["request"])) {
                Ok(response) => response,
                Err(error) => return failure("native_failure", &error.to_string()),
            };
        if let Err(error) = validate_envelope(&response) {
            return error;
        }
        return match session.submit_request(request, runner) {
            Ok(task_id) => json::object! { version: VERSION, ok: true, task_id: task_id },
            Err(message) => failure("submission_failed", &message),
        };
    }
    let Some(task_id) = value["task_id"].as_u32() else {
        return failure("invalid_input", "Invalid task_id");
    };
    if !session.is_task_initiated(task_id)
        && !session.is_sent_to_runner(task_id)
        && !runner.has_diagnostics(id, task_id)
    {
        return failure("task_not_found", "Task does not exist");
    }
    let mut document = runner.diagnostics(id, task_id);
    document["version"] = VERSION.into();
    document["ok"] = true.into();
    document
}

pub fn handle(document: &str, manager: &Arc<SessionManager>, runner: &Arc<Runner>) -> String {
    format!("OK {}\n", dispatch(document, manager, runner).dump())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn status_requires_no_session_and_rejects_session_fields() {
        let manager = SessionManager::new(100);
        let runner = Runner::new();
        let status = dispatch(r#"{"version":2,"command":"status"}"#, &manager, &runner);
        assert_eq!(status["ok"], true);
        assert_eq!(status["status"]["status_reason"], "runner_not_running");
        let error = dispatch(
            r#"{"version":2,"command":"status","session_id":0}"#,
            &manager,
            &runner,
        );
        assert_eq!(error["error"]["code"], "invalid_input");
    }

    #[test]
    fn validation_envelopes_reject_missing_or_partial_errors() {
        for value in [
            "null",
            "[]",
            "{}",
            r#"{"schema_version":2,"ok":false}"#,
            r#"{"schema_version":2,"ok":false,"error":{"code":"invalid_input"}}"#,
            r#"{"schema_version":2,"ok":"yes"}"#,
            r#"{"schema_version":1,"ok":true}"#,
        ] {
            assert_eq!(
                validate_envelope(value).unwrap_err()["error"]["code"],
                "native_failure"
            );
        }
        let error = validate_envelope(r#"{"schema_version":2,"ok":false,"error":{"code":"invalid_input","message":"bad arity"}}"#).unwrap_err();
        assert_eq!(error["error"]["code"], "invalid_input");
        assert_eq!(error["error"]["message"], "bad arity");
        assert!(validate_envelope(r#"{"schema_version":2,"ok":true,"valid":true}"#).is_ok());
        assert!(
            validate_envelope(r#"{"schema_version":2,"ok":true,"operation":"batch","results":[]}"#)
                .is_ok()
        );
    }

    #[test]
    fn malformed_or_invalid_submission_is_atomic() {
        let manager = SessionManager::new(100);
        let runner = Runner::new();
        let id = manager.create_session();
        for document in [
            "{".to_owned(),
            r#"{"version":1,"command":"capabilities"}"#.to_owned(),
            json::object! { version: 2, command: "submit", session_id: id,
            request: { schema_version: 2, operation: "execute" } }
            .dump(),
        ] {
            assert_eq!(
                dispatch(&document, &manager, &runner)["ok"].as_bool(),
                Some(false)
            );
        }
        assert!(!manager.get_session(id).unwrap().is_sent_to_runner(0));
    }

    #[test]
    fn queued_and_unsubmitted_cancellation_keeps_logs_scoped_to_session() {
        let manager = SessionManager::new(1);
        let runner = Runner::new();
        let id = manager.create_session();
        let other_id = manager.create_session();
        let session = manager.get_session(id).unwrap();
        let logs = |session_id: u32, task_id: u32| {
            dispatch(
                &json::object! {version:2, command:"logs", session_id:session_id, task_id:task_id}
                    .dump(),
                &manager,
                &runner,
            )
        };
        for queued in [false, true] {
            let task = session.initiate_task();
            if queued {
                session.set_task_qubits(task, 1);
                session.set_task_qasm(task, "OPENQASM 2.0; qreg q[1];".into());
                assert_eq!(
                    session.handle_request(vec![&task.to_string(), "EXECUTE"], Arc::clone(&runner)),
                    "OK\n"
                );
                assert_eq!(logs(id, task)["events"][0]["state"], "queued");
            }
            assert!(session.cancel_task(Arc::clone(&runner), task).unwrap());
            let document = logs(id, task);
            assert_eq!(document["ok"], true);
            assert_eq!(document["logs"], "");
            assert_eq!(document["logs_truncated"], false);
            assert_eq!(document["logs_omitted_bytes"], 0);
            let states: Vec<_> = document["events"]
                .members()
                .map(|event| event["state"].as_str().unwrap())
                .collect();
            assert_eq!(
                states,
                if queued {
                    vec!["queued", "cancelled"]
                } else {
                    vec!["cancelled"]
                }
            );
            assert_eq!(logs(other_id, task)["error"]["code"], "task_not_found");
            session.tick();
            session.tick();
            assert!(session.timed_out(1));
            assert_eq!(logs(id, task)["ok"], true);
            assert!(!session.timed_out(1));
        }
        assert_eq!(logs(id, u32::MAX)["error"]["code"], "task_not_found");
        manager.remove_session(id, Arc::clone(&runner)).unwrap();
        assert!(!runner.has_diagnostics(id, 0));
        assert_eq!(logs(id, 0)["error"]["code"], "session_not_found");
    }

    #[test]
    fn capabilities_and_keepalive_have_versioned_envelopes() {
        let manager = SessionManager::new(1);
        let runner = Runner::new();
        let id = manager.create_session();
        let caps = dispatch(
            r#"{"version":2,"command":"capabilities"}"#,
            &manager,
            &runner,
        );
        assert_eq!(caps["ok"].as_bool(), Some(true));
        assert_eq!(caps["native"]["python_required"].as_bool(), Some(false));
        let session = manager.get_session(id).unwrap();
        session.tick();
        let alive = dispatch(
            &json::object! {version:2, command:"keepalive", session_id:id}.dump(),
            &manager,
            &runner,
        );
        assert_eq!(alive["ok"].as_bool(), Some(true));
        session.tick();
        assert!(!session.timed_out(1));
    }
    #[test]
    fn native_noise_and_failed_tasks_keep_diagnostics_after_consumption() {
        let manager = SessionManager::new(100);
        let runner = Runner::new();
        let thread = Runner::run(Arc::clone(&runner));
        let id = manager.create_session();
        let request = json::object! {schema_version:2, operation:"estimate",
        circuit:{num_qubits:1, source:"OPENQASM 2.0;\n// comment\nqreg q[1]; h q[0];"},
        simulator:{backend:"qcsim", method:"density_matrix"}, observables:["X","Z"],
        noise:{evaluation:"exact", channels:[{kind:"t1", targets:[0], gamma:0.36}]}};
        let submit = dispatch(
            &json::object! {version:2,command:"submit",session_id:id,request:request}.dump(),
            &manager,
            &runner,
        );
        assert_eq!(submit["ok"].as_bool(), Some(true), "{submit}");
        let task = submit["task_id"].as_u32().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !runner.is_task_result_available(id, task) {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let result = runner.get_result(id, task).unwrap();
        assert!(result.error.is_none(), "{:?}", result.error);
        let value = json::parse(&result.result).unwrap();
        assert!((value["expectation_values"][0].as_f64().unwrap() - 0.8).abs() < 1e-9);
        assert!((value["expectation_values"][1].as_f64().unwrap() - 0.36).abs() < 1e-9);
        assert!(runner.has_diagnostics(id, task));
        let logs = dispatch(
            &json::object! {version:2,command:"logs",session_id:id,task_id:task}.dump(),
            &manager,
            &runner,
        );
        assert_eq!(logs["ok"].as_bool(), Some(true));
        runner.clear_session_diagnostics(id);
        assert!(!runner.has_diagnostics(id, task));
        runner.stop().unwrap();
        thread.join().unwrap();
    }
}
