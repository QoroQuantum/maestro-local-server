//! Private, versioned protocol between the daemon and its one-task worker.
//!
//! Each JSON message has an eight-byte big-endian length prefix. JSON escaping
//! preserves QASM, observables, and options containing newlines or quotes.

use std::io::{self, Read, Write};

use crate::maestro::runner::{Task, TaskType};

const VERSION: u32 = 1;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn write_message(writer: &mut impl Write, message: json::JsonValue) -> io::Result<()> {
    let payload = message.dump();
    writer.write_all(&(payload.len() as u64).to_be_bytes())?;
    writer.write_all(payload.as_bytes())
}

pub(super) fn read_message(reader: &mut impl Read) -> io::Result<json::JsonValue> {
    let mut header = [0; 8];
    reader.read_exact(&mut header)?;
    let length = u64::from_be_bytes(header);
    // Read incrementally instead of allocating an untrusted advertised length.
    let mut payload = String::new();
    reader.take(length).read_to_string(&mut payload)?;
    if payload.len() as u64 != length {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Truncated worker message",
        ));
    }
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
            task_type: match task.task_type { TaskType::Execute => "execute", TaskType::Estimate => "estimate" },
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

pub(super) fn write_result(writer: &mut impl Write, task: &Task, result: &str) -> io::Result<()> {
    write_message(
        writer,
        json::object! {
            version: VERSION,
            session_id: task.session_id,
            task_id: task.task_id,
            result: result,
        },
    )
}

pub(super) fn read_result(reader: &mut impl Read, task: &Task) -> io::Result<String> {
    let message = read_message(reader)?;
    if message["session_id"].as_u32() != Some(task.session_id)
        || message["task_id"].as_u32() != Some(task.task_id)
    {
        return Err(invalid("Worker result belongs to a different task"));
    }
    let result = message["result"]
        .as_str()
        .ok_or_else(|| invalid("Missing worker result"))?;
    if result.is_empty() {
        return Err(invalid("Maestro returned an empty result"));
    }
    // Native results must be complete JSON, not a log line or a partial result.
    json::parse(result).map_err(|error| invalid(format!("Invalid Maestro result: {error}")))?;
    Ok(result.to_owned())
}
