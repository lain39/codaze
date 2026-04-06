use super::*;
use crate::gateway_errors::parse_retry_after;
use crate::models::ModelsSnapshot;
use crate::request_normalization::{
    GATEWAY_CONTROL_FIELD, SESSION_SOURCE_HEADER, apply_body_gateway_overrides,
    normalize_compact_request_body, normalize_responses_request_body,
};
use crate::responses::extract_retry_after;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

fn test_models_snapshot() -> ModelsSnapshot {
    ModelsSnapshot::from_value(json!({
        "models": [
            {
                "slug": "gpt-5.4",
                "display_name": "GPT-5.4",
                "description": null,
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [],
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": 1,
                "availability_nux": null,
                "upgrade": null,
                "base_instructions": "",
                "model_messages": null,
                "supports_reasoning_summaries": false,
                "default_reasoning_summary": "auto",
                "support_verbosity": false,
                "default_verbosity": null,
                "apply_patch_tool_type": null,
                "web_search_tool_type": "text",
                "truncation_policy": { "mode": "bytes", "limit": 10000 },
                "supports_parallel_tool_calls": true,
                "supports_image_detail_original": false,
                "context_window": 272000,
                "auto_compact_token_limit": null,
                "effective_context_window_percent": 95,
                "experimental_supported_tools": [],
                "input_modalities": ["text", "image"],
                "used_fallback_model_metadata": false,
                "supports_search_tool": false
            },
            {
                "slug": "gpt-5",
                "display_name": "GPT-5",
                "description": null,
                "default_reasoning_level": "medium",
                "supported_reasoning_levels": [],
                "shell_type": "shell_command",
                "visibility": "list",
                "supported_in_api": true,
                "priority": 1,
                "availability_nux": null,
                "upgrade": null,
                "base_instructions": "",
                "model_messages": null,
                "supports_reasoning_summaries": false,
                "default_reasoning_summary": "auto",
                "support_verbosity": false,
                "default_verbosity": null,
                "apply_patch_tool_type": null,
                "web_search_tool_type": "text",
                "truncation_policy": { "mode": "bytes", "limit": 10000 },
                "supports_parallel_tool_calls": false,
                "supports_image_detail_original": false,
                "context_window": 272000,
                "auto_compact_token_limit": null,
                "effective_context_window_percent": 95,
                "experimental_supported_tools": [],
                "input_modalities": ["text", "image"],
                "used_fallback_model_metadata": false,
                "supports_search_tool": false
            }
        ]
    }))
    .expect("snapshot")
}

#[test]
fn gateway_session_source_object_is_serialized_and_stripped() {
    let mut headers = HeaderMap::new();
    let mut body = json!({
        "_gateway": {
            "session_source": { "subagent": "review" }
        }
    });

    apply_body_gateway_overrides(&mut headers, &mut body).expect("should succeed");

    assert_eq!(
        headers
            .get(SESSION_SOURCE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(r#"{"subagent":"review"}"#)
    );
    assert!(body.get(GATEWAY_CONTROL_FIELD).is_none());
}

#[test]
fn gateway_session_source_string_is_serialized_as_session_source_json() {
    let mut headers = HeaderMap::new();
    let mut body = json!({
        "_gateway": {
            "session_source": "exec"
        }
    });

    apply_body_gateway_overrides(&mut headers, &mut body).expect("should succeed");

    assert_eq!(
        headers
            .get(SESSION_SOURCE_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(r#""exec""#)
    );
}

#[test]
fn body_session_source_rejects_numbers() {
    let mut headers = HeaderMap::new();
    let mut body = json!({
        "_gateway": {
            "session_source": 7
        }
    });

    let error =
        apply_body_gateway_overrides(&mut headers, &mut body).expect_err("should reject numbers");

    assert!(error.contains("SessionSource JSON"));
}

#[test]
fn parse_retry_after_from_header_value() {
    let value = HeaderValue::from_static("42");
    assert_eq!(parse_retry_after(&value), Some(Duration::from_secs(42)));
}

#[test]
fn parse_retry_after_from_http_date_header_value() {
    let future = (chrono::Utc::now() + chrono::Duration::seconds(90)).to_rfc2822();
    let value = HeaderValue::from_str(&future).expect("valid header value");
    let parsed = parse_retry_after(&value).expect("parsed retry-after");

    assert!(parsed.as_secs() <= 90);
    assert!(parsed.as_secs() >= 89);
}

#[test]
fn extract_retry_after_uses_retry_after_header_before_body_resets() {
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: Some({
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_static("11"));
            headers
        }),
        body: Some(
            r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#
                .to_string(),
        ),
    };

    assert_eq!(extract_retry_after(&error), Some(Duration::from_secs(11)));
}

#[test]
fn extract_retry_after_falls_back_to_http_body_resets_at() {
    let resets_at = chrono::Utc::now().timestamp() + 90;
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: None,
        body: Some(format!(
            r#"{{"error":{{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_at":{resets_at}}}}}"#
        )),
    };

    let retry_after = extract_retry_after(&error).expect("retry after");
    assert!(retry_after.as_secs() <= 90);
    assert!(retry_after.as_secs() >= 89);
}

#[test]
fn normalize_responses_defaults_add_store_and_parallel_tool_calls() {
    let mut body = json!({
        "model": "gpt-5.4"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(FingerprintMode::Normalize, &mut body, Some(&snapshot));

    assert_eq!(body["instructions"], Value::String(String::new()));
    assert_eq!(body["store"], Value::Bool(false));
    assert_eq!(body["parallel_tool_calls"], Value::Bool(true));
}

#[test]
fn normalize_responses_defaults_do_not_override_existing_values() {
    let mut body = json!({
        "instructions": "keep me",
        "store": true,
        "parallel_tool_calls": false
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(FingerprintMode::Normalize, &mut body, Some(&snapshot));

    assert_eq!(body["instructions"], Value::String("keep me".to_string()));
    assert_eq!(body["store"], Value::Bool(true));
    assert_eq!(body["parallel_tool_calls"], Value::Bool(false));
}

#[test]
fn normalize_responses_null_instructions_becomes_empty_string() {
    let mut body = json!({
        "instructions": null,
        "model": "gpt-5.4"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(FingerprintMode::Normalize, &mut body, Some(&snapshot));

    assert_eq!(body["instructions"], Value::String(String::new()));
}

#[test]
fn normalize_compact_defaults_parallel_tool_calls_from_models_snapshot() {
    let mut body = json!({
        "model": "gpt-5"
    });

    let snapshot = test_models_snapshot();
    normalize_compact_request_body(FingerprintMode::Normalize, &mut body, Some(&snapshot));

    assert_eq!(body["instructions"], Value::String(String::new()));
    assert_eq!(body["parallel_tool_calls"], Value::Bool(false));
    assert!(body.get("store").is_none());
}

#[test]
fn normalize_compact_null_instructions_becomes_empty_string() {
    let mut body = json!({
        "instructions": null,
        "model": "gpt-5"
    });

    let snapshot = test_models_snapshot();
    normalize_compact_request_body(FingerprintMode::Normalize, &mut body, Some(&snapshot));

    assert_eq!(body["instructions"], Value::String(String::new()));
}

#[test]
fn passthrough_does_not_inject_defaults() {
    let mut responses_body = json!({
        "model": "gpt-5.4"
    });
    let mut compact_body = json!({
        "model": "gpt-5.4"
    });

    normalize_responses_request_body(FingerprintMode::Passthrough, &mut responses_body, None);
    normalize_compact_request_body(FingerprintMode::Passthrough, &mut compact_body, None);

    assert!(responses_body.get("store").is_none());
    assert!(responses_body.get("parallel_tool_calls").is_none());
    assert!(compact_body.get("store").is_none());
    assert!(compact_body.get("parallel_tool_calls").is_none());
}

#[test]
fn unknown_models_default_parallel_tool_calls_to_true() {
    let mut body = json!({
        "model": "future-model"
    });

    normalize_compact_request_body(FingerprintMode::Normalize, &mut body, None);

    assert_eq!(body["parallel_tool_calls"], Value::Bool(true));
}
