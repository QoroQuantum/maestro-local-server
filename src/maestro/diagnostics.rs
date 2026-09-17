//! Bounded output, lifecycle records and error chains for public task logs.
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

const OUTPUT_HALF_BYTES: usize = 32 * 1024;
const OUTPUT_MARKER: &str = "\n[... Maestro log output truncated ...]\n";

/// Keep startup context and recent output without letting a noisy worker grow
/// daemon memory indefinitely. Limits count raw bytes, before lossy UTF-8 decoding.
#[derive(Debug, Default)]
pub(super) struct CapturedOutput {
    prefix: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: u64,
}

impl CapturedOutput {
    pub fn append(&mut self, bytes: &[u8]) {
        let prefix_count = bytes.len().min(OUTPUT_HALF_BYTES - self.prefix.len());
        self.prefix.extend_from_slice(&bytes[..prefix_count]);
        let bytes = &bytes[prefix_count..];
        let overflow = (self.tail.len() + bytes.len()).saturating_sub(OUTPUT_HALF_BYTES);
        let skip = overflow.saturating_sub(self.tail.len());
        self.tail.drain(..overflow.min(self.tail.len()));
        self.tail.extend(&bytes[skip..]);
        self.omitted = self.omitted.saturating_add(overflow as u64);
    }

    pub fn text(&self) -> String {
        let mut bytes = self.prefix.clone();
        if self.omitted != 0 {
            bytes.extend_from_slice(OUTPUT_MARKER.as_bytes());
        }
        bytes.extend(&self.tail);
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[derive(Debug)]
pub(super) struct TaskDiagnostics {
    pub output: Arc<Mutex<CapturedOutput>>,
    pub error: Option<String>,
    pub context: json::JsonValue,
    events: Vec<json::JsonValue>,
    events_truncated: bool,
}

impl Default for TaskDiagnostics {
    fn default() -> Self {
        Self {
            output: Arc::default(),
            error: None,
            context: json::object! {},
            events: Vec::new(),
            events_truncated: false,
        }
    }
}

impl TaskDiagnostics {
    pub fn new(context: json::JsonValue) -> Self {
        Self {
            context,
            ..Default::default()
        }
    }

    pub fn event(&mut self, state: &str) {
        // Retries cannot produce an unbounded history or repeat a transition.
        if self
            .events
            .last()
            .is_some_and(|event| event["state"] == state)
        {
            return;
        }
        if self.events.len() == 8 {
            self.events.remove(1); // Keep the first and the most recent transitions.
            self.events_truncated = true;
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        self.events
            .push(json::object! { timestamp_unix_ms: timestamp, state: state });
    }

    pub fn document(&self, session_id: u32, task_id: u32) -> json::JsonValue {
        let output = self.output.lock().unwrap();
        let error = self
            .error
            .as_deref()
            .and_then(|message| json::parse(message).ok())
            .map(|value| value["error"].clone())
            .unwrap_or(json::JsonValue::Null);
        json::object! {
            session_id: session_id, task_id: task_id,
            logs: output.text(), logs_truncated: output.omitted != 0,
            logs_omitted_bytes: output.omitted,
            error: error, failure_message: self.error.clone(),
            context: self.context.clone(), events: self.events.clone(),
            events_truncated: self.events_truncated,
        }
    }
}

/// Copy only bounded identifiers, never circuits or arbitrary native options.
pub(super) fn task_context(task: &super::runner::Task) -> json::JsonValue {
    use super::runner::TaskType;
    if matches!(
        task.task_type,
        TaskType::Request | TaskType::ValidateRequest
    ) {
        let request = json::parse(&task.qasm).unwrap_or(json::JsonValue::Null);
        let mut context = json::object! {};
        if let Some(operation) = request["operation"].as_str() {
            context["operation"] = bounded(operation, 256).into();
        }
        for field in ["backend", "method"] {
            if let Some(value) = request["simulator"][field].as_str() {
                context["simulator"][field] = bounded(value, 256).into();
            }
        }
        if let Some(profile) = request["launch"]["profile"].as_str() {
            context["launch"]["profile"] = bounded(profile, 256).into();
        }
        if let Some(ranks) = request["launch"]["ranks"].as_u32() {
            context["launch"]["ranks"] = ranks.into();
        }
        context
    } else {
        json::object! {
            operation: if matches!(task.task_type, TaskType::Execute) { "execute" } else { "estimate" },
            simulator: { type_id: task.sim_type, method_id: task.method },
        }
    }
}

const MESSAGE_BYTES: usize = 8192;
const MAX_CAUSES: usize = 4;
const MAX_DEPTH: usize = 3;

fn bounded(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        return value.to_owned();
    }
    let mut end = limit;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} [truncated]", &value[..end])
}

fn normalize(value: &json::JsonValue, depth: usize) -> json::JsonValue {
    let mut detail = json::object! {
        code: bounded(value["code"].as_str().unwrap_or("worker_failure"), 256),
        message: bounded(value["message"].as_str().unwrap_or("Unknown worker failure"), MESSAGE_BYTES),
    };
    if let Some(confirmed) = value["cleanup_confirmed"].as_bool() {
        detail["cleanup_confirmed"] = confirmed.into();
    }
    if depth < MAX_DEPTH && value["causes"].is_array() {
        detail["causes"] = value["causes"]
            .members()
            .take(MAX_CAUSES)
            .map(|cause| normalize(cause, depth + 1))
            .collect::<Vec<_>>()
            .into();
    }
    if value["causes"].len() > MAX_CAUSES || (depth == MAX_DEPTH && !value["causes"].is_empty()) {
        detail["causes_truncated"] = true.into();
    }
    if value["causes_truncated"].as_bool() == Some(true) {
        detail["causes_truncated"] = true.into();
    }
    detail
}

fn detail(error: &io::Error, fallback: &str) -> json::JsonValue {
    let text = error.to_string();
    if let Ok(value) = json::parse(&text)
        && value["error"]["code"].is_string()
        && value["error"]["message"].is_string()
    {
        return normalize(&value["error"], 0);
    }
    json::object! {code: fallback, message: bounded(&text, MESSAGE_BYTES)}
}

fn document(detail: json::JsonValue) -> String {
    json::object! {schema_version:2, ok:false, error:detail}.dump()
}

pub(super) fn envelope(error: &io::Error) -> String {
    document(detail(error, "worker_failure"))
}

pub(super) fn native_error(result: &str) -> Option<io::Error> {
    let value = json::parse(result).ok()?;
    (value["ok"].as_bool() == Some(false)
        && value["error"]["code"].is_string()
        && value["error"]["message"].is_string())
    .then(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            document(normalize(&value["error"], 0)),
        )
    })
}

pub(super) fn with_causes(error: io::Error, code: &str, causes: &[&io::Error]) -> io::Error {
    let mut primary = detail(&error, code);
    primary["code"] = code.into();
    if code == "cleanup_unconfirmed" {
        primary["cleanup_confirmed"] = false.into();
    }
    if !causes.is_empty() {
        primary["causes"] = causes
            .iter()
            .map(|cause| detail(cause, "worker_failure"))
            .collect::<Vec<_>>()
            .into();
    }
    io::Error::new(error.kind(), document(normalize(&primary, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_keeps_prefix_and_latest_tail_across_sizes_and_chunk_boundaries() {
        for size in [0, 1, 32768, 32769, 65536, 65537, 256 * 1024] {
            let bytes: Vec<u8> = (0..size).map(|i| b'a' + (i % 26) as u8).collect();
            for chunk_size in [7, 8192, 256 * 1024] {
                let mut output = CapturedOutput::default();
                for chunk in bytes.chunks(chunk_size) {
                    output.append(chunk);
                }
                let mut expected = bytes.clone();
                if size > 65536 {
                    expected = bytes[..32768].to_vec();
                    expected.extend_from_slice(OUTPUT_MARKER.as_bytes());
                    expected.extend_from_slice(&bytes[size - 32768..]);
                }
                assert_eq!(output.text().as_bytes(), expected);
                assert_eq!(output.omitted, size.saturating_sub(65536) as u64);
                assert!(output.prefix.len() + output.tail.len() <= 65536);
            }
        }
    }

    #[test]
    fn unicode_output_survives_reads_and_storage_boundaries() {
        let input = "a".repeat(32767) + &"é🌍".repeat(4000);
        let mut output = CapturedOutput::default();
        for byte in input.as_bytes() {
            output.append(&[*byte]);
        }
        assert_eq!(output.text(), input);
        output.append(&[0xff; 65536]);
        let record = TaskDiagnostics {
            output: Arc::new(Mutex::new(output)),
            ..Default::default()
        };
        let document = record.document(1, 2);
        assert_eq!(document["logs_truncated"], true);
        assert_eq!(json::parse(&document.dump()).unwrap(), document);
    }

    #[test]
    fn context_and_repeated_transitions_are_bounded() {
        let mut task = super::super::worker::tests::task("");
        task.task_type = super::super::runner::TaskType::Request;
        task.qasm = json::object! {
            operation: "execute", simulator: {backend: "é".repeat(1024), method: "statevector", options: {secret: "excluded"}},
            circuit: {source: "excluded"}, launch: {profile: "cluster", ranks: 4},
        }.dump();
        let context = task_context(&task);
        assert!(!context.dump().contains("excluded"));
        assert!(context["simulator"]["backend"].as_str().unwrap().len() < 300);
        assert_eq!(context["launch"]["profile"], "cluster");
        assert_eq!(context["launch"]["ranks"], 4);
        let mut record = TaskDiagnostics {
            context,
            ..Default::default()
        };
        record.event("queued");
        record.event("queued");
        assert_eq!(record.document(1, 2)["events"].len(), 1);
        for _ in 0..10 {
            record.event("started");
            record.event("cancellation_failed");
        }
        record.event("cancelled");
        let value = record.document(1, 2);
        assert_eq!(value["events"].len(), 8);
        assert_eq!(value["events_truncated"], true);
        assert_eq!(value["events"][0]["state"], "queued");
        assert_eq!(value["events"][7]["state"], "cancelled");
    }

    #[test]
    fn cleanup_keeps_structured_native_and_protocol_causes() {
        let native =
            native_error(r#"{"ok":false,"error":{"code":"invalid_input","message":"bad gate"}}"#)
                .unwrap();
        let protocol = io::Error::new(io::ErrorKind::BrokenPipe, "closed input");
        let error = with_causes(
            io::Error::new(io::ErrorKind::TimedOut, "scancel timed out"),
            "cleanup_unconfirmed",
            &[&native, &protocol],
        );
        let value = json::parse(&envelope(&error)).unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(value["error"]["cleanup_confirmed"], false);
        assert_eq!(value["error"]["causes"][0]["code"], "invalid_input");
        assert_eq!(value["error"]["causes"][1]["message"], "closed input");
    }

    #[test]
    fn large_unicode_error_trees_are_bounded_without_invalid_utf8() {
        let mut detail = json::object! {code:"native", message:"leaf"};
        for _ in 0..5 {
            detail = json::object! {code:"outer", message:"cause", causes:vec![detail; 5]};
        }
        let error = io::Error::other(document(detail));
        let encoded = envelope(&error);
        assert!(encoded.len() < 1024 * 1024);
        let value = json::parse(&encoded).unwrap();
        assert_eq!(value["error"]["causes"].len(), MAX_CAUSES);
        assert_eq!(value["error"]["causes_truncated"], true);
        let leaf = io::Error::other("é".repeat(MESSAGE_BYTES));
        assert!(
            json::parse(&envelope(&leaf)).unwrap()["error"]["message"]
                .as_str()
                .unwrap()
                .ends_with(" [truncated]")
        );
    }
}
