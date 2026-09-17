//! Bounded error chains shared by the supervisor, worker and public task logs.
use std::io;

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
