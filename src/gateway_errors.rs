use crate::accounts::{BlockedReason, PoolBlockSummary};
use crate::classifier::classify_request_error;
#[cfg(test)]
use crate::error_semantics::parse_retry_after_str;
use crate::error_semantics::parse_structured_error_value;
use crate::failover::FailoverFailure;
use crate::http_shape::shape_openai_http_headers;
use crate::models::ResponseShape;
use crate::responses::SyntheticResponseFailedPayload;
use crate::responses::fallback_error_code_for_http;
use crate::responses::render_synthetic_response_failed_event;
use crate::responses::synthetic_response_failed_payload_from_http_failure;
use crate::responses::synthetic_response_failed_payload_from_transport;
use crate::responses::{
    DownstreamFailureKind, GATEWAY_UNAVAILABLE_MESSAGE, downstream_failure_kind,
    downstream_failure_kind_for_http, gateway_unavailable_payload,
};
use crate::upstream::{RefreshFailure, sanitize_response_headers};
use axum::Json;
use axum::body::Body;
#[cfg(test)]
use axum::http::HeaderValue;
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use serde_json::{Value, json};
#[cfg(test)]
use std::time::Duration;
use tracing::warn;

pub(crate) const JSON_CONTENT_TYPE: &str = "application/json";
pub(crate) const SSE_CONTENT_TYPE: &str = "text/event-stream";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailureRenderMode {
    UnaryJson,
    ResponsesPreStream,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InternalFailoverSurface {
    CodexUnary,
    OpenAiUnary,
    CodexResponsesPreStream,
    OpenAiResponsesPreStream,
    WebsocketFailover,
}

impl InternalFailoverSurface {
    fn as_str(self) -> &'static str {
        match self {
            Self::CodexUnary => "codex_unary",
            Self::OpenAiUnary => "openai_unary",
            Self::CodexResponsesPreStream => "codex_responses_pre_stream",
            Self::OpenAiResponsesPreStream => "openai_responses_pre_stream",
            Self::WebsocketFailover => "websocket_failover",
        }
    }
}

pub(crate) fn json_error(status: StatusCode, message: String) -> Response {
    (status, Json(json!({ "error": { "message": message } }))).into_response()
}

pub(crate) fn render_status_message_error(
    shape: ResponseShape,
    mode: FailureRenderMode,
    status: StatusCode,
    message: String,
) -> Response {
    render_failover_failure(
        &FailoverFailure::CallerJson { status, message },
        shape,
        mode,
    )
}

pub(crate) fn render_failover_failure(
    error: &FailoverFailure,
    shape: ResponseShape,
    mode: FailureRenderMode,
) -> Response {
    if let FailoverFailure::Internal { status, detail } = error {
        log_internal_failover_error(
            *status,
            detail,
            match (shape, mode) {
                (ResponseShape::Codex, FailureRenderMode::UnaryJson) => {
                    InternalFailoverSurface::CodexUnary
                }
                (ResponseShape::OpenAi, FailureRenderMode::UnaryJson) => {
                    InternalFailoverSurface::OpenAiUnary
                }
                (ResponseShape::Codex, FailureRenderMode::ResponsesPreStream) => {
                    InternalFailoverSurface::CodexResponsesPreStream
                }
                (ResponseShape::OpenAi, FailureRenderMode::ResponsesPreStream) => {
                    InternalFailoverSurface::OpenAiResponsesPreStream
                }
            },
        );
    }
    if failover_failure_kind(error) == DownstreamFailureKind::GatewayUnavailable {
        log_gateway_unavailable(error);
    }

    match mode {
        FailureRenderMode::UnaryJson => render_unary_failover_failure(error, shape),
        FailureRenderMode::ResponsesPreStream => match shape {
            ResponseShape::Codex => render_codex_pre_stream_failure(error),
            ResponseShape::OpenAi => render_unary_failover_failure(error, ResponseShape::OpenAi),
        },
    }
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
    structured_error_response(status, payload)
}

pub(crate) fn openai_pool_blocked_response(summary: PoolBlockSummary) -> Response {
    let status = status_for_pool_blocked_reason(summary.blocked_reason);
    let payload = synthetic_payload_for_pool_block(&summary);
    openai_error_response(status, HeaderMap::new(), payload)
}

pub(crate) fn transport_error_response_ref(error: &codex_client::TransportError) -> Response {
    match error {
        codex_client::TransportError::Http {
            status,
            body,
            headers,
            ..
        } => {
            let body = body.clone().unwrap_or_default();
            let forwarded = headers
                .as_ref()
                .map(sanitize_response_headers)
                .unwrap_or_default();
            (*status, forwarded, body).into_response()
        }
        other => json_error(StatusCode::BAD_GATEWAY, other.to_string()),
    }
}

pub(crate) fn openai_refresh_failure_response(error: &RefreshFailure) -> Response {
    openai_error_response_from_upstream_body(error.status, HeaderMap::new(), Some(&error.body))
}

pub(crate) fn openai_transport_error_response_ref(
    error: &codex_client::TransportError,
) -> Response {
    match error {
        codex_client::TransportError::Http {
            status,
            body,
            headers,
            ..
        } => {
            let forwarded = headers
                .as_ref()
                .map(sanitize_response_headers)
                .unwrap_or_default();
            openai_error_response_from_upstream_body(*status, forwarded, body.as_deref())
        }
        _ => openai_json_error(
            StatusCode::BAD_GATEWAY,
            public_internal_error_message(StatusCode::BAD_GATEWAY),
        ),
    }
}

fn structured_error_response(
    status: StatusCode,
    payload: SyntheticResponseFailedPayload,
) -> Response {
    let body = structured_error_body_from_payload(payload);
    (status, HeaderMap::new(), Json(body)).into_response()
}

pub(crate) fn set_content_type(headers: &mut HeaderMap, value: &str) {
    headers.remove(CONTENT_TYPE);
    insert_header(headers, CONTENT_TYPE.as_str(), value);
}

pub(crate) fn openai_json_error(status: StatusCode, message: String) -> Response {
    openai_json_error_with_headers(status, HeaderMap::new(), message, None)
}

fn openai_json_error_with_headers(
    status: StatusCode,
    headers: HeaderMap,
    message: String,
    code: Option<String>,
) -> Response {
    openai_error_response(
        status,
        headers,
        SyntheticResponseFailedPayload {
            code,
            message: Some(message),
            ..SyntheticResponseFailedPayload::default()
        },
    )
}

fn openai_error_response(
    status: StatusCode,
    headers: HeaderMap,
    payload: SyntheticResponseFailedPayload,
) -> Response {
    let mut headers = shape_openai_http_headers(headers);
    let body = openai_error_body(status, payload);
    set_content_type(&mut headers, JSON_CONTENT_TYPE);
    (status, headers, Json(body)).into_response()
}

fn openai_error_response_from_upstream_body(
    status: StatusCode,
    headers: HeaderMap,
    body: Option<&str>,
) -> Response {
    let mut headers = shape_openai_http_headers(headers);
    let parsed_json = body.and_then(|body| serde_json::from_str::<Value>(body).ok());
    if let Some(json_body) = parsed_json {
        set_content_type(&mut headers, JSON_CONTENT_TYPE);
        if is_passthrough_openai_error_body(&json_body) {
            return (
                status,
                headers,
                Json(sanitize_passthrough_openai_error_body(
                    status, &json_body, body,
                )),
            )
                .into_response();
        }
        return openai_error_response(
            status,
            headers,
            openai_error_payload_from_upstream_json(status, body, &json_body),
        );
    }

    let message = body
        .map(str::trim)
        .filter(|body| !body.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Upstream request failed")
                .to_string()
        });
    openai_json_error_with_headers(status, headers, message, None)
}

fn openai_error_payload_from_upstream_json(
    status: StatusCode,
    body: Option<&str>,
    json_body: &Value,
) -> SyntheticResponseFailedPayload {
    let source = json_body.get("error").unwrap_or(json_body);
    let payload = parse_structured_error_value(source);
    let message = payload.message.or_else(|| match json_body {
        Value::String(value) => Some(value.clone()),
        Value::Object(_) => json_body
            .get("detail")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        other => Some(other.to_string()),
    });

    SyntheticResponseFailedPayload {
        code: payload
            .code
            .or_else(|| Some(fallback_error_code_for_http(status, body).to_string())),
        message,
        error_type: payload.error_type,
        plan_type: payload.plan_type,
        resets_at: payload.resets_at,
        resets_in_seconds: payload.resets_in_seconds,
    }
}

fn is_passthrough_openai_error_body(json_body: &Value) -> bool {
    let Some(error) = json_body.get("error").and_then(Value::as_object) else {
        return false;
    };
    if error.get("message").and_then(Value::as_str).is_none() {
        return false;
    }
    matches!(
        error.get("type").and_then(Value::as_str),
        Some(
            "authentication_error"
                | "permission_error"
                | "rate_limit_error"
                | "server_error"
                | "invalid_request_error"
        )
    )
}

fn sanitize_passthrough_openai_error_body(
    status: StatusCode,
    json_body: &Value,
    body: Option<&str>,
) -> Value {
    let Some(error) = json_body.get("error").and_then(Value::as_object) else {
        return openai_error_body(
            status,
            openai_error_payload_from_upstream_json(status, body, json_body),
        );
    };

    let message = error
        .get("message")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Upstream request failed")
                .to_string()
        });
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| fallback_error_code_for_http(status, body).to_string());
    let error_type = normalize_openai_error_type(
        error.get("type").and_then(Value::as_str),
        status,
        Some(code.as_str()),
    );
    let param = error.get("param").cloned().unwrap_or(Value::Null);

    json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": param,
            "code": code,
        }
    })
}

fn render_unary_failover_failure(error: &FailoverFailure, shape: ResponseShape) -> Response {
    if failover_failure_kind(error) == DownstreamFailureKind::GatewayUnavailable {
        return render_gateway_unavailable_response(shape);
    }

    match shape {
        ResponseShape::Codex => match error {
            FailoverFailure::Refresh(error) => refresh_failure_response(error),
            FailoverFailure::Transport(error) => transport_error_response_ref(error),
            FailoverFailure::PoolBlocked(summary) => pool_blocked_response(summary.clone()),
            FailoverFailure::CallerJson { status, message } => json_error(*status, message.clone()),
            FailoverFailure::Internal { status, detail } => {
                let _ = detail;
                json_error(*status, public_internal_error_message(*status))
            }
        },
        ResponseShape::OpenAi => match error {
            FailoverFailure::Refresh(error) => openai_refresh_failure_response(error),
            FailoverFailure::Transport(error) => openai_transport_error_response_ref(error),
            FailoverFailure::PoolBlocked(summary) => openai_pool_blocked_response(summary.clone()),
            FailoverFailure::CallerJson { status, message } => {
                openai_json_error(*status, message.clone())
            }
            FailoverFailure::Internal { status, detail } => {
                let _ = detail;
                openai_json_error(*status, public_internal_error_message(*status))
            }
        },
    }
}

fn render_codex_pre_stream_failure(error: &FailoverFailure) -> Response {
    let payload = if failover_failure_kind(error) == DownstreamFailureKind::GatewayUnavailable {
        gateway_unavailable_payload()
    } else {
        match error {
            FailoverFailure::Refresh(error) => synthetic_response_failed_payload_from_http_failure(
                error.status,
                Some(error.body.as_str()),
                error.retry_after,
            ),
            FailoverFailure::Transport(error) => {
                synthetic_response_failed_payload_from_transport(error)
            }
            FailoverFailure::PoolBlocked(summary) => synthetic_payload_for_pool_block(summary),
            FailoverFailure::CallerJson { status, message } => SyntheticResponseFailedPayload {
                code: Some(fallback_error_code_for_http(*status, None).to_string()),
                message: Some(message.clone()),
                error_type: None,
                plan_type: None,
                resets_at: None,
                resets_in_seconds: None,
            },
            FailoverFailure::Internal { status, detail } => {
                let _ = detail;
                SyntheticResponseFailedPayload {
                    code: Some(fallback_error_code_for_http(*status, None).to_string()),
                    message: Some(public_internal_error_message(*status)),
                    error_type: None,
                    plan_type: None,
                    resets_at: None,
                    resets_in_seconds: None,
                }
            }
        }
    };

    let body = render_synthetic_response_failed_event(None, None, payload).unwrap_or_else(|| {
        Bytes::from_static(b"event: response.failed\ndata: {\"type\":\"response.failed\"}\n\n")
    });

    let mut headers = HeaderMap::new();
    insert_header(&mut headers, "content-type", SSE_CONTENT_TYPE);
    insert_header(&mut headers, "cache-control", "no-cache");
    (StatusCode::OK, headers, Body::from(body)).into_response()
}

fn openai_error_body(status: StatusCode, payload: SyntheticResponseFailedPayload) -> Value {
    let message = payload.message.unwrap_or_else(|| {
        status
            .canonical_reason()
            .unwrap_or("gateway request failed")
            .to_string()
    });
    let code = payload
        .code
        .unwrap_or_else(|| fallback_error_code_for_http(status, None).to_string());
    let error_type =
        normalize_openai_error_type(payload.error_type.as_deref(), status, Some(code.as_str()));

    json!({
        "error": {
            "message": message,
            "type": error_type,
            "param": Value::Null,
            "code": code,
        }
    })
}

fn normalize_openai_error_type(
    error_type: Option<&str>,
    status: StatusCode,
    code: Option<&str>,
) -> String {
    match error_type {
        Some(
            "authentication_error"
            | "permission_error"
            | "rate_limit_error"
            | "server_error"
            | "invalid_request_error",
        ) => error_type.unwrap_or_default().to_string(),
        _ => default_openai_error_type(status, code).to_string(),
    }
}

fn render_gateway_unavailable_response(shape: ResponseShape) -> Response {
    match shape {
        ResponseShape::Codex => structured_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            gateway_unavailable_payload(),
        ),
        ResponseShape::OpenAi => openai_error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            HeaderMap::new(),
            gateway_unavailable_payload(),
        ),
    }
}

fn default_openai_error_type(status: StatusCode, code: Option<&str>) -> &'static str {
    match (status.as_u16(), code) {
        (401, _) | (_, Some("invalid_api_key")) => "authentication_error",
        (403, _) | (_, Some("insufficient_quota" | "forbidden")) => "permission_error",
        (429, _) => "rate_limit_error",
        (500..=599, _) => "server_error",
        _ => "invalid_request_error",
    }
}

fn public_internal_error_message(status: StatusCode) -> String {
    status
        .canonical_reason()
        .unwrap_or("Internal Server Error")
        .to_string()
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        name.parse::<axum::http::HeaderName>(),
        axum::http::HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}

fn failover_failure_kind(error: &FailoverFailure) -> DownstreamFailureKind {
    match error {
        FailoverFailure::Refresh(_) => DownstreamFailureKind::GatewayUnavailable,
        FailoverFailure::Transport(codex_client::TransportError::Http { status, body, .. }) => {
            downstream_failure_kind_for_http(*status, body.as_deref())
        }
        FailoverFailure::Transport(error) => downstream_failure_kind(classify_request_error(error)),
        FailoverFailure::PoolBlocked(_) => DownstreamFailureKind::GatewayUnavailable,
        FailoverFailure::CallerJson { .. } | FailoverFailure::Internal { .. } => {
            DownstreamFailureKind::CallerError
        }
    }
}

fn log_gateway_unavailable(error: &FailoverFailure) {
    let source = match error {
        FailoverFailure::Refresh(error) => format!("refresh:{:?}", error.class),
        FailoverFailure::Transport(error) => {
            format!("transport:{:?}", classify_request_error(error))
        }
        FailoverFailure::PoolBlocked(summary) => format!("pool:{:?}", summary.blocked_reason),
        FailoverFailure::CallerJson { status, .. } => format!("caller_json:{}", status.as_u16()),
        FailoverFailure::Internal { status, .. } => format!("internal:{}", status.as_u16()),
    };
    warn!(
        status = %StatusCode::SERVICE_UNAVAILABLE,
        message = GATEWAY_UNAVAILABLE_MESSAGE,
        source = %source,
        "gateway unavailable"
    );
}

pub(crate) fn log_internal_failover_error(
    status: StatusCode,
    detail: &str,
    surface: InternalFailoverSurface,
) {
    warn!(
        status = %status,
        surface = surface.as_str(),
        detail = %detail,
        "internal failover error"
    );
}

pub(crate) fn synthetic_payload_for_pool_block(
    summary: &PoolBlockSummary,
) -> SyntheticResponseFailedPayload {
    let _ = summary;
    gateway_unavailable_payload()
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    #[tokio::test]
    async fn openai_transport_error_response_overrides_existing_content_type() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(
                    CONTENT_TYPE,
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                headers
            }),
            body: Some("{\"error\":{\"message\":\"bad request\"}}".to_string()),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes)
                .expect("json")
                .pointer("/error/message")
                .and_then(Value::as_str),
            Some("bad request")
        );
    }

    #[tokio::test]
    async fn openai_transport_error_response_removes_content_encoding() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(
                    axum::http::header::CONTENT_ENCODING,
                    HeaderValue::from_static("zstd"),
                );
                headers
            }),
            body: Some("{\"error\":{\"message\":\"bad request\"}}".to_string()),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert!(
            parts
                .headers
                .get(axum::http::header::CONTENT_ENCODING)
                .is_none()
        );
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad request")
        );
    }

    #[tokio::test]
    async fn openai_transport_error_response_filters_non_openai_headers() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(
                    axum::http::header::CACHE_CONTROL,
                    HeaderValue::from_static("no-store"),
                );
                headers.insert(
                    "x-codex-turn-state",
                    HeaderValue::from_static("turn-state-123"),
                );
                headers.insert("traceparent", HeaderValue::from_static("00-abc-123-01"));
                headers
            }),
            body: Some("{\"error\":{\"message\":\"bad request\"}}".to_string()),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            parts
                .headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(parts.headers.get("x-codex-turn-state").is_none());
        assert!(parts.headers.get("traceparent").is_none());
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad request")
        );
    }

    #[tokio::test]
    async fn openai_transport_error_response_wraps_non_enveloped_json() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers
            }),
            body: Some("{\"detail\":\"bad request\"}".to_string()),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad request")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert!(json.pointer("/detail").is_none());
    }

    #[tokio::test]
    async fn openai_refresh_failure_response_wraps_non_enveloped_json() {
        let response = openai_refresh_failure_response(&RefreshFailure {
            status: StatusCode::BAD_REQUEST,
            body: "{\"detail\":\"bad refresh request\"}".to_string(),
            class: crate::classifier::FailureClass::RequestRejected,
            retry_after: None,
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad refresh request")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert!(json.pointer("/detail").is_none());
    }

    #[tokio::test]
    async fn openai_transport_error_response_preserves_standard_openai_error_fields() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(
                    axum::http::header::CACHE_CONTROL,
                    HeaderValue::from_static("no-store"),
                );
                headers
            }),
            body: Some(
                "{\"error\":{\"message\":\"bad tool config\",\"type\":\"invalid_request_error\",\"param\":\"tools\",\"code\":\"invalid_value\"}}"
                    .to_string(),
            ),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            parts
                .headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad tool config")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            json.pointer("/error/param").and_then(Value::as_str),
            Some("tools")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("invalid_value")
        );
    }

    #[tokio::test]
    async fn openai_transport_error_response_strips_nonstandard_fields_from_standard_error_shape() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers
            }),
            body: Some(
                "{\"error\":{\"message\":\"bad tool config\",\"type\":\"invalid_request_error\",\"param\":\"tools\",\"code\":\"invalid_value\",\"resets_in_seconds\":77},\"plan_type\":\"plus\"}"
                    .to_string(),
            ),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad tool config")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
        assert_eq!(
            json.pointer("/error/param").and_then(Value::as_str),
            Some("tools")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("invalid_value")
        );
        assert!(json.pointer("/error/resets_in_seconds").is_none());
        assert!(json.pointer("/plan_type").is_none());
    }

    #[tokio::test]
    async fn openai_transport_error_response_rewraps_enveloped_non_openai_error_shape() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Http {
            status: StatusCode::BAD_GATEWAY,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                headers.insert(
                    axum::http::header::CACHE_CONTROL,
                    HeaderValue::from_static("no-store"),
                );
                headers
            }),
            body: Some(
                "{\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\",\"resets_in_seconds\":77}}"
                    .to_string(),
            ),
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            parts
                .headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("The usage limit has been reached")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("permission_error")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("insufficient_quota")
        );
        assert!(json.pointer("/error/param").is_some());
        assert!(json.pointer("/error/resets_in_seconds").is_none());
    }

    #[tokio::test]
    async fn openai_refresh_failure_response_rewraps_enveloped_non_openai_error_shape() {
        let response = openai_refresh_failure_response(&RefreshFailure {
            status: StatusCode::BAD_GATEWAY,
            body: "{\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\",\"resets_in_seconds\":77}}".to_string(),
            class: crate::classifier::FailureClass::QuotaExhausted,
            retry_after: None,
        });

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("The usage limit has been reached")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("permission_error")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("insufficient_quota")
        );
        assert!(json.pointer("/error/resets_in_seconds").is_none());
    }

    #[tokio::test]
    async fn openai_transport_error_response_hides_non_http_transport_details() {
        let response = openai_transport_error_response_ref(&codex_client::TransportError::Build(
            "tls config failed: /tmp/secret.pem".to_string(),
        ));

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_GATEWAY);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("Bad Gateway")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("server_error")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("internal_server_error")
        );
        assert!(
            !String::from_utf8(bytes.to_vec())
                .expect("utf8")
                .contains("/tmp/secret.pem")
        );
    }

    #[test]
    fn set_content_type_replaces_existing_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );

        set_content_type(&mut headers, SSE_CONTENT_TYPE);

        assert_eq!(
            headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(SSE_CONTENT_TYPE)
        );
    }

    #[tokio::test]
    async fn render_status_message_error_uses_sse_for_codex_responses_pre_stream() {
        let response = render_status_message_error(
            ResponseShape::Codex,
            FailureRenderMode::ResponsesPreStream,
            StatusCode::BAD_REQUEST,
            "bad request".to_string(),
        );

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");

        assert_eq!(parts.status, StatusCode::OK);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(SSE_CONTENT_TYPE)
        );
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");
        assert!(text.contains("event: response.failed"));
        assert!(text.contains("\"message\":\"bad request\""));
    }

    #[tokio::test]
    async fn render_status_message_error_uses_json_for_openai_responses_pre_stream() {
        let response = render_status_message_error(
            ResponseShape::OpenAi,
            FailureRenderMode::ResponsesPreStream,
            StatusCode::BAD_REQUEST,
            "bad request".to_string(),
        );

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::BAD_REQUEST);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("bad request")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("invalid_request_error")
        );
    }

    #[tokio::test]
    async fn render_status_message_error_preserves_json_internal_for_openai_responses_pre_stream() {
        let response = render_status_message_error(
            ResponseShape::OpenAi,
            FailureRenderMode::ResponsesPreStream,
            StatusCode::INTERNAL_SERVER_ERROR,
            "disk write failed".to_string(),
        );

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(parts.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            json.pointer("/error/message").and_then(Value::as_str),
            Some("disk write failed")
        );
        assert_eq!(
            json.pointer("/error/type").and_then(Value::as_str),
            Some("server_error")
        );
        assert_eq!(
            json.pointer("/error/code").and_then(Value::as_str),
            Some("internal_server_error")
        );
    }

    #[tokio::test]
    async fn render_status_message_error_preserves_json_internal_for_codex_responses_pre_stream() {
        let response = render_status_message_error(
            ResponseShape::Codex,
            FailureRenderMode::ResponsesPreStream,
            StatusCode::INTERNAL_SERVER_ERROR,
            "disk write failed".to_string(),
        );

        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let text = String::from_utf8(bytes.to_vec()).expect("utf8");

        assert_eq!(parts.status, StatusCode::OK);
        assert_eq!(
            parts
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some(SSE_CONTENT_TYPE)
        );
        assert!(text.contains("event: response.failed"));
        assert!(text.contains("\"message\":\"disk write failed\""));
        assert!(text.contains("\"code\":\"internal_server_error\""));
        assert!(!text.contains("\"code\":\"server_is_overloaded\""));
    }
}
