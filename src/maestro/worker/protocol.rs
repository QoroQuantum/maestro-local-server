//! Private, versioned protocol between the daemon and its one-task worker.
//!
//! Each frame has an eight-byte big-endian length prefix. Control messages are
//! JSON, whose escaping preserves QASM, observables, and options containing
//! newlines or quotes. A result follows its JSON header as a raw frame: escaping
//! it as a JSON string would inflate a quote-heavy native result past its limit.

use std::io::{self, Read, Write};

use crate::maestro::api::MAX_RESULT_BYTES;
use crate::maestro::runner::{Task, TaskType};

// Version 2 moved results out of the JSON header into their own frame.
const VERSION: u32 = 2;
// A task carries at most a 16 MiB request, which escaping can at most double.
const MAX_MESSAGE_BYTES: u64 = 64 * 1024 * 1024;

// A task is never sent until the worker has installed cancellation handlers.
pub(super) fn write_ready(writer: &mut impl Write) -> io::Result<()> {
    write_message(writer, json::object! {version: VERSION, ready: true})
}

pub(super) fn read_ready(reader: &mut impl Read) -> io::Result<()> {
    let message = read_message(reader)?;
    if message["ready"].as_bool() != Some(true) {
        return Err(invalid("Worker did not acknowledge readiness"));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn write_frame(writer: &mut impl Write, payload: &str, limit: u64) -> io::Result<()> {
    if payload.len() as u64 > limit {
        return Err(invalid("Worker message exceeds size limit"));
    }
    writer.write_all(&(payload.len() as u64).to_be_bytes())?;
    writer.write_all(payload.as_bytes())
}

fn read_frame(reader: &mut impl Read, limit: u64) -> io::Result<String> {
    let mut header = [0; 8];
    reader.read_exact(&mut header)?;
    let length = u64::from_be_bytes(header);
    if length > limit {
        return Err(invalid("Worker message exceeds size limit"));
    }
    // Read incrementally instead of allocating an untrusted advertised length.
    let mut payload = String::new();
    reader.take(length).read_to_string(&mut payload)?;
    if payload.len() as u64 != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Truncated worker message",
        ));
    }
    Ok(payload)
}

pub(super) fn write_message(writer: &mut impl Write, message: json::JsonValue) -> io::Result<()> {
    write_frame(writer, &message.dump(), MAX_MESSAGE_BYTES)
}

pub(super) fn read_message(reader: &mut impl Read) -> io::Result<json::JsonValue> {
    let payload = read_frame(reader, MAX_MESSAGE_BYTES)?;
    let message = json::parse(&payload).map_err(|error| invalid(error.to_string()))?;
    if message["version"].as_u32() != Some(VERSION) {
        return Err(invalid("Unsupported worker protocol version"));
    }
    Ok(message)
}

pub(super) fn write_task(writer: &mut impl Write, task: &Task) -> io::Result<()> {
    write_message(
        writer,
        json::object! {
            version: VERSION,
            session_id: task.session_id,
            task_id: task.task_id,
            task_type: match task.task_type { TaskType::Execute => "execute", TaskType::Estimate => "estimate", TaskType::Request => "request", TaskType::ValidateRequest => "validate_request" },
            qasm: task.qasm.as_str(),
            observables: task.observables.as_str(),
            nr_qubits: task.nr_qubits,
            sim_type: task.sim_type,
            method: task.method,
            options: task.options.as_str(),
        },
    )
}

pub(super) fn read_task(reader: &mut impl Read) -> io::Result<Task> {
    let message = read_message(reader)?;
    let number = |name: &str| {
        message[name]
            .as_u32()
            .ok_or_else(|| invalid(format!("Invalid {name}")))
    };
    let integer = |name: &str| {
        message[name]
            .as_i32()
            .ok_or_else(|| invalid(format!("Invalid {name}")))
    };
    let string = |name: &str| {
        message[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| invalid(format!("Invalid {name}")))
    };
    let task_type = match message["task_type"].as_str() {
        Some("execute") => TaskType::Execute,
        Some("estimate") => TaskType::Estimate,
        Some("request") => TaskType::Request,
        Some("validate_request") => TaskType::ValidateRequest,
        _ => return Err(invalid("Invalid task_type")),
    };
    Ok(Task::new(
        number("session_id")?,
        number("task_id")?,
        task_type,
        string("qasm")?,
        string("observables")?,
        integer("nr_qubits")?,
        integer("sim_type")?,
        integer("method")?,
        string("options")?,
    ))
}

/// `native_build` identifies the Maestro library the worker itself loaded. It is
/// absent when that library did not compute the result (MPI ranks run their own
/// worker executable). The result follows the header in its own raw frame.
pub(super) fn write_result(
    writer: &mut impl Write,
    task: &Task,
    result: &str,
    native_build: Option<&json::JsonValue>,
) -> io::Result<()> {
    // Refuse before writing, so the parent never sees a header without a result.
    if result.len() > MAX_RESULT_BYTES {
        return Err(invalid("Worker result exceeds size limit"));
    }
    let mut message = json::object! {
        version: VERSION,
        session_id: task.session_id,
        task_id: task.task_id,
    };
    if let Some(build) = native_build {
        message["native_build"] = build.clone();
    }
    write_message(writer, message)?;
    write_frame(writer, result, MAX_RESULT_BYTES as u64)
}

pub(super) struct WorkerResult {
    pub result: String,
    pub native_build: Option<json::JsonValue>,
}

// Retain only bounded identifiers, never an arbitrary worker document.
fn native_build(value: &json::JsonValue) -> Option<json::JsonValue> {
    let version = value["version"].as_str()?;
    let revision = value["source_revision"].as_str()?;
    Some(json::object! {
        version: crate::maestro::diagnostics::bounded(version, 256),
        source_revision: crate::maestro::diagnostics::bounded(revision, 256),
    })
}

pub(super) fn read_result(reader: &mut impl Read, task: &Task) -> io::Result<WorkerResult> {
    let message = read_message(reader)?;
    if message["session_id"].as_u32() != Some(task.session_id)
        || message["task_id"].as_u32() != Some(task.task_id)
    {
        return Err(invalid("Worker result belongs to a different task"));
    }
    let result = read_frame(reader, MAX_RESULT_BYTES as u64)?;
    if result.is_empty() {
        return Err(invalid("Maestro returned an empty result"));
    }
    // Native results must be complete JSON, not a log line or a partial result.
    json::parse(&result).map_err(|error| invalid(format!("Invalid Maestro result: {error}")))?;
    Ok(WorkerResult {
        result,
        native_build: native_build(&message["native_build"]),
    })
}
