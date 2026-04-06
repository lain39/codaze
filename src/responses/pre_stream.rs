use crate::failover::FailoverFailure;
use crate::gateway_errors::{
    json_error, pool_blocked_response, refresh_failure_response, synthetic_payload_for_pool_block,
    transport_error_response_ref,
};
use axum::body::Body;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;

use super::{
    SyntheticResponseFailedPayload, fallback_error_code_for_http,
    render_synthetic_response_failed_event, synthetic_response_failed_payload_from_http_failure,
    synthetic_response_failed_payload_from_transport,
};

pub(crate) fn responses_pre_stream_failure_response(
    error: &FailoverFailure,
    codex_originator: bool,
) -> Response {
    if !codex_originator {
        return match error {
            FailoverFailure::Refresh(error) => refresh_failure_response(error),
            FailoverFailure::Transport(error) => transport_error_response_ref(error),
            FailoverFailure::PoolBlocked(summary) => pool_blocked_response(summary.clone()),
            FailoverFailure::Json { status, message } => json_error(*status, message.clone()),
        };
    }

    let payload = match error {
        FailoverFailure::Refresh(error) => synthetic_response_failed_payload_from_http_failure(
            error.status,
            Some(error.body.as_str()),
            error.retry_after,
        ),
        FailoverFailure::Transport(error) => {
            synthetic_response_failed_payload_from_transport(error)
        }
        FailoverFailure::PoolBlocked(summary) => synthetic_payload_for_pool_block(summary),
        FailoverFailure::Json { status, message } => SyntheticResponseFailedPayload {
            code: Some(fallback_error_code_for_http(*status, None).to_string()),
            message: Some(message.clone()),
            error_type: None,
            plan_type: None,
            resets_at: None,
            resets_in_seconds: None,
        },
    };

    let body = render_synthetic_response_failed_event(None, None, payload).unwrap_or_else(|| {
        Bytes::from_static(b"event: response.failed\ndata: {\"type\":\"response.failed\"}\n\n")
    });

    let mut headers = HeaderMap::new();
    insert_header(&mut headers, "content-type", "text/event-stream");
    insert_header(&mut headers, "cache-control", "no-cache");
    (StatusCode::OK, headers, Body::from(body)).into_response()
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) = (
        name.parse::<axum::http::HeaderName>(),
        axum::http::HeaderValue::from_str(value),
    ) {
        headers.insert(header_name, header_value);
    }
}
