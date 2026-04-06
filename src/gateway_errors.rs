use crate::accounts::{BlockedReason, PoolBlockSummary};
#[cfg(test)]
use crate::error_semantics::parse_retry_after_str;
use crate::responses::SyntheticResponseFailedPayload;
use crate::upstream::{RefreshFailure, sanitize_response_headers};
use axum::Json;
#[cfg(test)]
use axum::http::HeaderValue;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::time::Duration;
use tracing::warn;

pub(crate) const CLIENT_NO_ACCOUNT_AVAILABLE_MESSAGE: &str =
    "No account available right now. Try again later.";

pub(crate) fn json_error(status: StatusCode, message: String) -> Response {
    warn!(%status, %message, "gateway request failed");
    (status, Json(json!({ "error": { "message": message } }))).into_response()
}

pub(crate) fn refresh_failure_response(error: &RefreshFailure) -> Response {
    match serde_json::from_str::<Value>(&error.body) {
        Ok(json_body) => (error.status, Json(json_body)).into_response(),
        Err(_) => json_error(error.status, error.body.clone()),
    }
}

pub(crate) fn pool_blocked_response(summary: PoolBlockSummary) -> Response {
    let status = status_for_pool_blocked_reason(summary.blocked_reason);
    let payload = synthetic_payload_for_pool_block(&summary);
    structured_error_response(status, payload, None)
}

pub(crate) fn transport_error_response(error: codex_client::TransportError) -> Response {
    match error {
        codex_client::TransportError::Http {
            status,
            body,
            headers,
            ..
        } => {
            let body = body.unwrap_or_default();
            let forwarded = headers
                .as_ref()
                .map(sanitize_response_headers)
                .unwrap_or_default();
            (status, forwarded, body).into_response()
        }
        other => json_error(StatusCode::BAD_GATEWAY, other.to_string()),
    }
}

fn structured_error_response(
    status: StatusCode,
    payload: SyntheticResponseFailedPayload,
    retry_after: Option<Duration>,
) -> Response {
    let body = structured_error_body_from_payload(payload);
    let message = body
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("gateway request failed")
        .to_string();
    warn!(%status, %message, "gateway request failed");
    let mut headers = HeaderMap::new();
    if let Some(retry_after) = retry_after {
        insert_retry_after_header(&mut headers, retry_after);
    }
    (status, headers, Json(body)).into_response()
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        name.parse::<axum::http::HeaderName>(),
        axum::http::HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}

fn insert_retry_after_header(headers: &mut HeaderMap, retry_after: Duration) {
    let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
    insert_header(headers, "retry-after", &seconds.max(1).to_string());
}

pub(crate) fn synthetic_payload_for_pool_block(
    summary: &PoolBlockSummary,
) -> SyntheticResponseFailedPayload {
    let _ = summary;
    SyntheticResponseFailedPayload {
        code: Some("server_is_overloaded".to_string()),
        message: Some(CLIENT_NO_ACCOUNT_AVAILABLE_MESSAGE.to_string()),
        error_type: None,
        plan_type: None,
        resets_at: None,
        resets_in_seconds: None,
    }
}

fn status_for_pool_blocked_reason(_reason: BlockedReason) -> StatusCode {
    StatusCode::SERVICE_UNAVAILABLE
}

fn structured_error_body_from_payload(payload: SyntheticResponseFailedPayload) -> Value {
    let message = payload
        .message
        .unwrap_or_else(|| "Upstream request failed.".to_string());
    let code = payload
        .code
        .unwrap_or_else(|| "internal_server_error".to_string());

    let mut error_object = serde_json::Map::from_iter([
        ("code".to_string(), Value::String(code)),
        ("message".to_string(), Value::String(message)),
    ]);
    if let Some(error_type) = payload.error_type {
        error_object.insert("type".to_string(), Value::String(error_type));
    }
    if let Some(plan_type) = payload.plan_type {
        error_object.insert("plan_type".to_string(), Value::String(plan_type));
    }
    if let Some(resets_at) = payload.resets_at {
        error_object.insert(
            "resets_at".to_string(),
            Value::Number(serde_json::Number::from(resets_at)),
        );
    }
    if let Some(resets_in_seconds) = payload.resets_in_seconds {
        error_object.insert(
            "resets_in_seconds".to_string(),
            Value::Number(serde_json::Number::from(resets_in_seconds)),
        );
    }

    json!({ "error": Value::Object(error_object) })
}

#[cfg(test)]
pub(crate) fn parse_retry_after(value: &HeaderValue) -> Option<Duration> {
    parse_retry_after_str(value.to_str().ok()?.trim())
}
