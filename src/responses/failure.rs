use crate::classifier::{FailureClass, classify_request_http};
use crate::error_semantics::{
    parse_resets_retry_after_payload, parse_retry_after_header, parse_structured_error_body,
};
use bytes::Bytes;
use http::{HeaderMap, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Default)]
pub(crate) struct SyntheticResponseFailedPayload {
    pub(crate) code: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) error_type: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) resets_at: Option<i64>,
    pub(crate) resets_in_seconds: Option<i64>,
}

pub(crate) fn extract_retry_after(error: &codex_client::TransportError) -> Option<Duration> {
    let codex_client::TransportError::Http { headers, body, .. } = error else {
        return None;
    };
    extract_http_retry_after(headers.as_ref(), body.as_deref())
}

pub(crate) fn extract_http_retry_after(
    headers: Option<&HeaderMap>,
    body: Option<&str>,
) -> Option<Duration> {
    parse_retry_after_header(headers).or_else(|| extract_http_body_retry_after(body))
}

pub(crate) fn render_synthetic_response_failed_event(
    last_response_id: Option<&str>,
    last_sequence_number: Option<i64>,
    payload: SyntheticResponseFailedPayload,
) -> Option<Bytes> {
    let payload = synthetic_response_failed_json(last_response_id, last_sequence_number, payload);
    let encoded = serde_json::to_string(&payload).ok()?;
    Some(Bytes::from(format!(
        "event: response.failed\ndata: {encoded}\n\n"
    )))
}

pub(crate) fn synthetic_response_failed_payload_from_transport(
    error: &codex_client::TransportError,
) -> SyntheticResponseFailedPayload {
    let extracted = extract_synthetic_response_failed_payload(error);
    SyntheticResponseFailedPayload {
        code: extracted
            .code
            .or_else(|| Some(fallback_error_code(error).to_string())),
        message: extracted
            .message
            .or_else(|| Some(fallback_error_message(error, extract_retry_after(error)))),
        error_type: extracted.error_type,
        plan_type: extracted.plan_type,
        resets_at: extracted.resets_at,
        resets_in_seconds: extracted.resets_in_seconds,
    }
}

pub(crate) fn synthetic_response_failed_payload_from_http_failure(
    status: StatusCode,
    body: Option<&str>,
    retry_after: Option<Duration>,
) -> SyntheticResponseFailedPayload {
    let extracted = extract_synthetic_response_failed_payload_from_body(body);
    SyntheticResponseFailedPayload {
        code: extracted
            .code
            .or_else(|| Some(fallback_error_code_for_http(status, body).to_string())),
        message: extracted
            .message
            .or_else(|| Some(fallback_error_message_for_http(status, retry_after))),
        error_type: extracted.error_type,
        plan_type: extracted.plan_type,
        resets_at: extracted.resets_at,
        resets_in_seconds: extracted.resets_in_seconds,
    }
}

pub(crate) fn fallback_error_code_for_http(status: StatusCode, body: Option<&str>) -> &'static str {
    match classify_request_http(status, body) {
        FailureClass::AccessTokenRejected | FailureClass::AuthInvalid => "invalid_api_key",
        FailureClass::RateLimited => "rate_limit_exceeded",
        FailureClass::QuotaExhausted => "insufficient_quota",
        FailureClass::RiskControlled => "forbidden",
        FailureClass::RequestRejected => match status.as_u16() {
            400 => "invalid_request_error",
            403 => "forbidden",
            404 => "not_found",
            408 => "request_timeout",
            409 => "conflict",
            500..=599 => "internal_server_error",
            _ => "http_error",
        },
        FailureClass::TemporaryFailure | FailureClass::InternalFailure => match status.as_u16() {
            408 => "request_timeout",
            500..=599 => "internal_server_error",
            _ => "http_error",
        },
    }
}

fn extract_http_body_retry_after(body: Option<&str>) -> Option<Duration> {
    parse_resets_retry_after_payload(&parse_structured_error_body(body))
}

fn synthetic_response_failed_json(
    last_response_id: Option<&str>,
    last_sequence_number: Option<i64>,
    extracted: SyntheticResponseFailedPayload,
) -> Value {
    let response_id = last_response_id
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| format!("resp_gateway_{}", chrono::Utc::now().timestamp_micros()));
    let sequence_number = last_sequence_number.unwrap_or(-1) + 1;
    let message = extracted
        .message
        .unwrap_or_else(|| "Upstream request failed.".to_string());
    let code = extracted
        .code
        .unwrap_or_else(|| "internal_server_error".to_string());

    let mut error_object = serde_json::Map::from_iter([
        ("code".to_string(), Value::String(code)),
        ("message".to_string(), Value::String(message)),
    ]);
    if let Some(error_type) = extracted.error_type {
        error_object.insert("type".to_string(), Value::String(error_type));
    }
    if let Some(plan_type) = extracted.plan_type {
        error_object.insert("plan_type".to_string(), Value::String(plan_type));
    }
    if let Some(resets_at) = extracted.resets_at {
        error_object.insert(
            "resets_at".to_string(),
            Value::Number(serde_json::Number::from(resets_at)),
        );
    }
    if let Some(resets_in_seconds) = extracted.resets_in_seconds {
        error_object.insert(
            "resets_in_seconds".to_string(),
            Value::Number(serde_json::Number::from(resets_in_seconds)),
        );
    }

    json!({
        "type": "response.failed",
        "sequence_number": sequence_number,
        "response": {
            "id": response_id,
            "object": "response",
            "created_at": chrono::Utc::now().timestamp(),
            "status": "failed",
            "background": false,
            "error": Value::Object(error_object),
            "incomplete_details": Value::Null,
        }
    })
}

fn extract_synthetic_response_failed_payload(
    error: &codex_client::TransportError,
) -> SyntheticResponseFailedPayload {
    let codex_client::TransportError::Http { body, .. } = error else {
        return SyntheticResponseFailedPayload::default();
    };
    extract_synthetic_response_failed_payload_from_body(body.as_deref())
}

fn extract_synthetic_response_failed_payload_from_body(
    body: Option<&str>,
) -> SyntheticResponseFailedPayload {
    let extracted = parse_structured_error_body(body);
    SyntheticResponseFailedPayload {
        code: extracted.code,
        message: extracted.message,
        error_type: extracted.error_type,
        plan_type: extracted.plan_type,
        resets_at: extracted.resets_at,
        resets_in_seconds: extracted.resets_in_seconds,
    }
}

fn fallback_error_code(error: &codex_client::TransportError) -> &'static str {
    match error {
        codex_client::TransportError::Http { status, body, .. } => {
            fallback_error_code_for_http(*status, body.as_deref())
        }
        codex_client::TransportError::Timeout => "request_timeout",
        codex_client::TransportError::Network(_) => "network_error",
        codex_client::TransportError::Build(_) => "internal_server_error",
        codex_client::TransportError::RetryLimit => "retry_limit_reached",
    }
}

fn fallback_error_message_for_http(status: StatusCode, retry_after: Option<Duration>) -> String {
    match retry_after {
        Some(delay) if status == StatusCode::TOO_MANY_REQUESTS => {
            format!(
                "Rate limit reached. Please try again in {}s.",
                delay.as_secs_f64()
            )
        }
        _ => format!("Upstream request failed with status {}.", status.as_u16()),
    }
}

fn fallback_error_message(
    error: &codex_client::TransportError,
    retry_after: Option<Duration>,
) -> String {
    match (error, retry_after) {
        (codex_client::TransportError::Http { status, .. }, _) => {
            fallback_error_message_for_http(*status, retry_after)
        }
        _ => error.to_string(),
    }
}
