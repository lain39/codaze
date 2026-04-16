use crate::config::FingerprintMode;
use crate::models::ModelsSnapshot;
use axum::http::HeaderMap;
use codex_protocol::protocol::SessionSource;
use serde_json::Map;
use serde_json::Value;

pub(crate) const GATEWAY_CONTROL_FIELD: &str = "_gateway";
#[cfg(test)]
pub(crate) const SESSION_SOURCE_HEADER: &str = "x-codex-session-source";

pub(crate) fn normalize_responses_request_body(
    mode: FingerprintMode,
    codex_originator: bool,
    body: &mut Value,
    snapshot: Option<&ModelsSnapshot>,
) {
    if let Some(object) = body.as_object_mut() {
        if !codex_originator {
            normalize_non_codex_responses_compatibility(object);
            normalize_string_input_to_message(object);
        }
        if mode != FingerprintMode::Normalize {
            return;
        }
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
    codex_originator: bool,
    body: &mut Value,
    snapshot: Option<&ModelsSnapshot>,
) {
    if let Some(object) = body.as_object_mut()
        && !codex_originator
    {
        normalize_string_input_to_message(object);
    }

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

fn normalize_string_input_to_message(object: &mut Map<String, Value>) {
    let Some(text) = object
        .get("input")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return;
    };

    object.insert(
        "input".to_string(),
        Value::Array(vec![Value::Object(Map::from_iter([
            ("type".to_string(), Value::String("message".to_string())),
            ("role".to_string(), Value::String("user".to_string())),
            (
                "content".to_string(),
                Value::Array(vec![Value::Object(Map::from_iter([
                    ("type".to_string(), Value::String("input_text".to_string())),
                    ("text".to_string(), Value::String(text)),
                ]))]),
            ),
        ]))]),
    );
}

fn normalize_non_codex_responses_compatibility(object: &mut Map<String, Value>) {
    for key in [
        "max_output_tokens",
        "max_completion_tokens",
        "temperature",
        "top_p",
        "truncation",
        "user",
    ] {
        object.remove(key);
    }

    if !matches!(
        object.get("service_tier"),
        Some(Value::String(value)) if value == "priority"
    ) {
        object.remove("service_tier");
    }

    normalize_builtin_tool_aliases_at_path(object.get_mut("tools"));
    normalize_tool_choice_aliases(object.get_mut("tool_choice"));
}

fn normalize_builtin_tool_aliases_at_path(value: Option<&mut Value>) {
    let Some(Value::Array(tools)) = value else {
        return;
    };
    for tool in tools {
        normalize_builtin_tool_alias(tool);
    }
}

fn normalize_tool_choice_aliases(value: Option<&mut Value>) {
    let Some(tool_choice) = value else {
        return;
    };
    match tool_choice {
        Value::String(_) => normalize_builtin_tool_choice_string(tool_choice),
        Value::Object(tool_choice) => {
            if let Some(value) = tool_choice.get_mut("type") {
                normalize_builtin_tool_alias_type(value);
            }
            normalize_builtin_tool_aliases_at_path(tool_choice.get_mut("tools"));
        }
        _ => {}
    }
}

fn normalize_builtin_tool_alias(tool: &mut Value) {
    let Some(tool) = tool.as_object_mut() else {
        return;
    };
    if let Some(value) = tool.get_mut("type") {
        normalize_builtin_tool_alias_type(value);
    }
}

fn normalize_builtin_tool_alias_type(value: &mut Value) {
    let Some(tool_type) = value.as_str() else {
        return;
    };
    let normalized = normalize_builtin_tool_type(tool_type);
    if let Some(normalized) = normalized {
        *value = Value::String(normalized.to_string());
    }
}

fn normalize_builtin_tool_choice_string(value: &mut Value) {
    let Some(tool_type) = value.as_str() else {
        return;
    };
    let normalized = match tool_type {
        "web_search" => Some("web_search"),
        other => normalize_builtin_tool_type(other),
    };
    if let Some(normalized) = normalized {
        *value = Value::Object(Map::from_iter([(
            "type".to_string(),
            Value::String(normalized.to_string()),
        )]));
    }
}

fn normalize_builtin_tool_type(tool_type: &str) -> Option<&'static str> {
    match tool_type {
        "web_search_preview" | "web_search_preview_2025_03_11" => Some("web_search"),
        _ => None,
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
