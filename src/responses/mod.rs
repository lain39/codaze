pub(crate) mod failure;
pub(crate) mod pre_stream;
pub(crate) mod stream;
pub(crate) mod websocket;

pub(crate) use self::failure::{
    SyntheticResponseFailedPayload, extract_retry_after, fallback_error_code_for_http,
    render_synthetic_response_failed_event, synthetic_response_failed_payload_from_http_failure,
    synthetic_response_failed_payload_from_transport,
};
pub(crate) use self::pre_stream::responses_pre_stream_failure_response;
pub(crate) use self::stream::ManagedResponseStream;
#[cfg(test)]
pub(crate) use self::stream::ResponsesSseState;
#[cfg(test)]
pub(crate) use self::websocket::{
    PendingWebsocketRequest, PendingWebsocketRetryResult, retry_pending_websocket_request,
};
#[cfg(test)]
pub(crate) use self::websocket::{
    WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE, WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE,
    classify_websocket_error_text, classify_websocket_upstream_message,
    is_responses_websocket_request_start, normalize_rate_limit_event_payload,
    normalize_response_create_installation_id_payload, rewrite_previous_response_not_found_message,
    rewrite_previous_response_not_found_payload, should_passthrough_retryable_websocket_reset,
    upstream_message_commits_request, upstream_message_is_terminal,
};
pub(crate) use self::websocket::{
    WebsocketProxyOutcome, classify_response_failed_event, proxy_websocket,
};
