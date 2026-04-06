use crate::config::FingerprintMode;
use crate::models::ModelsSnapshot;
use axum::http::HeaderMap;
use codex_protocol::protocol::SessionSource;
use serde_json::Value;

pub(crate) const GATEWAY_CONTROL_FIELD: &str = "_gateway";
#[cfg(test)]
pub(crate) const SESSION_SOURCE_HEADER: &str = "x-codex-session-source";

pub(crate) fn normalize_responses_request_body(
    mode: FingerprintMode,
    body: &mut Value,
    snapshot: Option<&ModelsSnapshot>,
) {
    if mode != FingerprintMode::Normalize {
        return;
    }

    if let Some(object) = body.as_object_mut() {
        normalize_instructions_field(object);
        let default_parallel_tool_calls = Value::Bool(crate::models::default_parallel_tool_calls(
            object.get("model"),
            snapshot,
        ));
        object
            .entry("store".to_string())
            .or_insert_with(|| Value::Bool(false));
        object
            .entry("parallel_tool_calls".to_string())
            .or_insert(default_parallel_tool_calls);
    }
}

pub(crate) fn normalize_compact_request_body(
    mode: FingerprintMode,
    body: &mut Value,
    snapshot: Option<&ModelsSnapshot>,
) {
    if mode != FingerprintMode::Normalize {
        return;
    }

    if let Some(object) = body.as_object_mut() {
        normalize_instructions_field(object);
        let default_parallel_tool_calls = Value::Bool(crate::models::default_parallel_tool_calls(
            object.get("model"),
            snapshot,
        ));
        object
            .entry("parallel_tool_calls".to_string())
            .or_insert(default_parallel_tool_calls);
    }
}

fn normalize_instructions_field(object: &mut serde_json::Map<String, Value>) {
    match object.get_mut("instructions") {
        Some(value) if value.is_null() => {
            *value = Value::String(String::new());
        }
        Some(_) => {}
        None => {
            object.insert("instructions".to_string(), Value::String(String::new()));
        }
    }
}

pub(crate) fn apply_body_gateway_overrides(
    headers: &mut HeaderMap,
    body: &mut Value,
) -> Result<(), String> {
    let Some(object) = body.as_object_mut() else {
        return Ok(());
    };

    let gateway = object.remove(GATEWAY_CONTROL_FIELD);
    let mut gateway_session_source = None;

    if let Some(gateway) = gateway {
        match gateway {
            Value::Null => {}
            Value::Object(mut gateway) => {
                gateway_session_source = gateway.remove("session_source");
            }
            other => {
                return Err(format!(
                    "_gateway must be null or object; got {}",
                    json_type_name(&other)
                ));
            }
        }
    }

    if let Some(session_source) = gateway_session_source {
        apply_session_source_override(headers, session_source)?;
    }

    Ok(())
}

fn apply_session_source_override(
    headers: &mut HeaderMap,
    session_source: Value,
) -> Result<(), String> {
    match session_source {
        Value::Null => {
            headers.remove("x-codex-session-source");
            Ok(())
        }
        other => {
            let parsed: SessionSource = serde_json::from_value(other.clone()).map_err(|error| {
                format!("session_source must match codex_protocol::SessionSource JSON: {error}")
            })?;
            let encoded = serde_json::to_string(&parsed)
                .map_err(|error| format!("invalid session_source: {error}"))?;
            insert_header(headers, "x-codex-session-source", &encoded);
            Ok(())
        }
    }
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        name.parse::<axum::http::HeaderName>(),
        axum::http::HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}

fn json_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
