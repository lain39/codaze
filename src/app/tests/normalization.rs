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
fn extract_retry_after_falls_back_to_message_retry_after() {
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: None,
        body: Some(
            r#"{"error":{"message":"Rate limit reached for gpt-5.4. Please try again in 11.5s.","code":"rate_limit_exceeded"}}"#
                .to_string(),
        ),
    };

    let retry_after = extract_retry_after(&error).expect("retry after");
    assert_eq!(retry_after.as_millis(), 11_500);
}

#[test]
fn extract_retry_after_falls_back_to_plain_text_message_retry_after() {
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: None,
        body: Some("Rate limit reached for gpt-5.4. Please try again in 8s.".to_string()),
    };

    let retry_after = extract_retry_after(&error).expect("retry after");
    assert_eq!(retry_after.as_secs(), 8);
}

#[test]
fn extract_retry_after_supports_milliseconds_in_message() {
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: None,
        body: Some(
            r#"{"error":{"message":"Rate limit reached for gpt-5.4. Please try again in 500 milliseconds.","code":"rate_limit_exceeded"}}"#
                .to_string(),
        ),
    };

    let retry_after = extract_retry_after(&error).expect("retry after");
    assert_eq!(retry_after, Duration::from_millis(500));
}

#[test]
fn normalize_responses_defaults_add_store_and_parallel_tool_calls() {
    let mut body = json!({
        "model": "gpt-5.4"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(FingerprintMode::Normalize, true, &mut body, Some(&snapshot));

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
    normalize_responses_request_body(FingerprintMode::Normalize, true, &mut body, Some(&snapshot));

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
    normalize_responses_request_body(FingerprintMode::Normalize, true, &mut body, Some(&snapshot));

    assert_eq!(body["instructions"], Value::String(String::new()));
}

#[test]
fn normalize_responses_non_codex_strips_rejected_fields_and_keeps_priority_service_tier() {
    let mut body = json!({
        "model": "gpt-5.4",
        "max_output_tokens": 10,
        "max_completion_tokens": 11,
        "temperature": 0.2,
        "top_p": 0.7,
        "truncation": "disabled",
        "user": "user-123",
        "service_tier": "priority"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(
        FingerprintMode::Normalize,
        false,
        &mut body,
        Some(&snapshot),
    );

    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("temperature").is_none());
    assert!(body.get("top_p").is_none());
    assert!(body.get("truncation").is_none());
    assert!(body.get("user").is_none());
    assert_eq!(body["service_tier"], Value::String("priority".to_string()));
}

#[test]
fn normalize_responses_non_codex_removes_non_priority_service_tier() {
    let mut body = json!({
        "model": "gpt-5.4",
        "service_tier": "auto"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(
        FingerprintMode::Normalize,
        false,
        &mut body,
        Some(&snapshot),
    );

    assert!(body.get("service_tier").is_none());
}

#[test]
fn normalize_responses_non_codex_rewrites_web_search_preview_aliases() {
    let mut body = json!({
        "model": "gpt-5.4",
        "tools": [
            { "type": "web_search_preview" },
            { "type": "web_search_preview_2025_03_11" }
        ],
        "tool_choice": {
            "type": "web_search_preview_2025_03_11",
            "tools": [
                { "type": "web_search_preview" }
            ]
        }
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(
        FingerprintMode::Normalize,
        false,
        &mut body,
        Some(&snapshot),
    );

    assert_eq!(
        body["tools"][0]["type"],
        Value::String("web_search".to_string())
    );
    assert_eq!(
        body["tools"][1]["type"],
        Value::String("web_search".to_string())
    );
    assert_eq!(
        body["tool_choice"]["type"],
        Value::String("web_search".to_string())
    );
    assert_eq!(
        body["tool_choice"]["tools"][0]["type"],
        Value::String("web_search".to_string())
    );
}

#[test]
fn normalize_responses_non_codex_rewrites_string_tool_choice_web_search_values() {
    let mut body = json!({
        "model": "gpt-5.4",
        "tool_choice": "web_search_preview_2025_03_11"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(
        FingerprintMode::Normalize,
        false,
        &mut body,
        Some(&snapshot),
    );

    assert_eq!(
        body["tool_choice"]["type"],
        Value::String("web_search".to_string())
    );

    let mut stable_body = json!({
        "model": "gpt-5.4",
        "tool_choice": "web_search"
    });

    normalize_responses_request_body(
        FingerprintMode::Normalize,
        false,
        &mut stable_body,
        Some(&snapshot),
    );

    assert_eq!(
        stable_body["tool_choice"]["type"],
        Value::String("web_search".to_string())
    );
}

#[test]
fn normalize_responses_non_codex_keeps_context_management_and_system_role() {
    let mut body = json!({
        "model": "gpt-5.4",
        "context_management": [
            { "type": "compaction", "compact_threshold": 12000 }
        ],
        "input": [
            {
                "role": "system",
                "content": [{ "type": "input_text", "text": "hi" }]
            }
        ]
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(
        FingerprintMode::Normalize,
        false,
        &mut body,
        Some(&snapshot),
    );

    assert_eq!(
        body["context_management"][0]["type"],
        Value::String("compaction".to_string())
    );
    assert_eq!(
        body["input"][0]["role"],
        Value::String("system".to_string())
    );
}

#[test]
fn normalize_responses_codex_originator_keeps_non_codex_compat_fields_untouched() {
    let mut body = json!({
        "model": "gpt-5.4",
        "service_tier": "auto",
        "tools": [{ "type": "web_search_preview" }],
        "user": "user-123"
    });

    let snapshot = test_models_snapshot();
    normalize_responses_request_body(FingerprintMode::Normalize, true, &mut body, Some(&snapshot));

    assert_eq!(body["service_tier"], Value::String("auto".to_string()));
    assert_eq!(
        body["tools"][0]["type"],
        Value::String("web_search_preview".to_string())
    );
    assert_eq!(body["user"], Value::String("user-123".to_string()));
}

#[test]
fn normalize_compact_defaults_parallel_tool_calls_from_models_snapshot() {
    let mut body = json!({
        "model": "gpt-5"
    });

    let snapshot = test_models_snapshot();
    normalize_compact_request_body(FingerprintMode::Normalize, true, &mut body, Some(&snapshot));

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
    normalize_compact_request_body(FingerprintMode::Normalize, true, &mut body, Some(&snapshot));

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

    normalize_responses_request_body(
        FingerprintMode::Passthrough,
        false,
        &mut responses_body,
        None,
    );
    normalize_compact_request_body(FingerprintMode::Passthrough, true, &mut compact_body, None);

    assert!(responses_body.get("store").is_none());
    assert!(responses_body.get("parallel_tool_calls").is_none());
    assert!(compact_body.get("store").is_none());
    assert!(compact_body.get("parallel_tool_calls").is_none());
}

#[test]
fn passthrough_still_applies_non_codex_compatibility_normalization() {
    let mut body = json!({
        "model": "gpt-5.4",
        "max_output_tokens": 10,
        "service_tier": "auto",
        "tool_choice": "web_search_preview",
        "user": "user-123"
    });

    normalize_responses_request_body(FingerprintMode::Passthrough, false, &mut body, None);

    assert!(body.get("max_output_tokens").is_none());
    assert!(body.get("service_tier").is_none());
    assert!(body.get("user").is_none());
    assert_eq!(
        body["tool_choice"]["type"],
        Value::String("web_search".to_string())
    );
    assert!(body.get("instructions").is_none());
    assert!(body.get("store").is_none());
    assert!(body.get("parallel_tool_calls").is_none());
}

#[test]
fn unknown_models_default_parallel_tool_calls_to_true() {
    let mut body = json!({
        "model": "future-model"
    });

    normalize_compact_request_body(FingerprintMode::Normalize, true, &mut body, None);

    assert_eq!(body["parallel_tool_calls"], Value::Bool(true));
}

#[test]
fn normalize_responses_non_codex_string_input_becomes_user_message() {
    let mut body = json!({
        "model": "gpt-5.4",
        "input": "hi"
    });

    normalize_responses_request_body(FingerprintMode::Normalize, false, &mut body, None);

    assert_eq!(
        body.pointer("/input/0/type").and_then(Value::as_str),
        Some("message")
    );
    assert_eq!(
        body.pointer("/input/0/role").and_then(Value::as_str),
        Some("user")
    );
    assert_eq!(
        body.pointer("/input/0/content/0/type")
            .and_then(Value::as_str),
        Some("input_text")
    );
    assert_eq!(
        body.pointer("/input/0/content/0/text")
            .and_then(Value::as_str),
        Some("hi")
    );
}

#[test]
fn normalize_responses_codex_string_input_is_unchanged() {
    let mut body = json!({
        "model": "gpt-5.4",
        "input": "hi"
    });

    normalize_responses_request_body(FingerprintMode::Normalize, true, &mut body, None);

    assert_eq!(body["input"], Value::String("hi".to_string()));
}

#[test]
fn normalize_compact_non_codex_string_input_becomes_user_message_even_in_passthrough() {
    let mut body = json!({
        "model": "gpt-5.4",
        "input": "hi"
    });

    normalize_compact_request_body(FingerprintMode::Passthrough, false, &mut body, None);

    assert_eq!(
        body.pointer("/input/0/type").and_then(Value::as_str),
        Some("message")
    );
    assert_eq!(
        body.pointer("/input/0/role").and_then(Value::as_str),
        Some("user")
    );
    assert_eq!(
        body.pointer("/input/0/content/0/type")
            .and_then(Value::as_str),
        Some("input_text")
    );
    assert_eq!(
        body.pointer("/input/0/content/0/text")
            .and_then(Value::as_str),
        Some("hi")
    );
    assert!(body.get("instructions").is_none());
    assert!(body.get("parallel_tool_calls").is_none());
}

#[test]
fn normalize_compact_codex_string_input_is_unchanged() {
    let mut body = json!({
        "model": "gpt-5.4",
        "input": "hi"
    });

    normalize_compact_request_body(FingerprintMode::Normalize, true, &mut body, None);

    assert_eq!(body["input"], Value::String("hi".to_string()));
}
