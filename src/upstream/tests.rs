use super::client::build_codex_user_agent;
use super::headers::{
    OPENAI_BETA_HEADER, RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE, SESSION_SOURCE_HEADER,
    build_models_extra_headers, build_responses_extra_headers, build_responses_websocket_headers,
    build_unary_extra_headers, parse_retry_after_header, sanitize_response_headers,
};
use super::http::{
    append_client_version_query, apply_http_request_timeout, configure_responses_stream_request,
};
use super::*;
use ::http::header::{ACCEPT, HeaderValue};

#[test]
fn appends_client_version_query() {
    let mut req = Request {
        method: Method::GET,
        url: "https://chatgpt.com/backend-api/codex/models".to_string(),
        headers: HeaderMap::new(),
        body: None,
        compression: RequestCompression::None,
        timeout: None,
    };

    append_client_version_query(&mut req, "0.117.0");

    assert_eq!(
        req.url,
        "https://chatgpt.com/backend-api/codex/models?client_version=0.117.0"
    );
}

#[test]
fn user_agent_uses_configured_codex_version() {
    let user_agent = build_codex_user_agent("0.117.0");
    let prefix = format!(
        "{}/0.117.0 ",
        codex_login::default_client::originator().value
    );

    assert!(user_agent.starts_with(&prefix));
    assert!(user_agent.ends_with(" (codex-tui; 0.117.0)"));
}

#[test]
fn models_headers_forward_incoming_fingerprint_in_passthrough_mode() {
    let mut incoming = HeaderMap::new();
    incoming.insert(
        "user-agent",
        HeaderValue::from_static("codex_cli_rs/0.117.0"),
    );
    incoming.insert("originator", HeaderValue::from_static("codex_cli_rs"));

    let headers = build_models_extra_headers(&incoming, FingerprintMode::Passthrough);

    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs/0.117.0")
    );
    assert_eq!(
        headers
            .get("originator")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs")
    );
}

#[test]
fn responses_headers_forward_incoming_fingerprint_in_passthrough_mode() {
    let mut incoming = HeaderMap::new();
    incoming.insert(
        "user-agent",
        HeaderValue::from_static("codex_cli_rs/0.117.0"),
    );
    incoming.insert("originator", HeaderValue::from_static("codex_cli_rs"));
    incoming.insert("session_id", HeaderValue::from_static("abc"));

    let headers = build_responses_extra_headers(&incoming, FingerprintMode::Passthrough);

    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs/0.117.0")
    );
    assert_eq!(
        headers
            .get("originator")
            .and_then(|value| value.to_str().ok()),
        Some("codex_cli_rs")
    );
    assert_eq!(
        headers
            .get("session_id")
            .and_then(|value| value.to_str().ok()),
        Some("abc")
    );
}

#[test]
fn responses_headers_forward_identity_headers_from_incoming_request() {
    let mut incoming = HeaderMap::new();
    incoming.insert(
        "x-codex-window-id",
        HeaderValue::from_static("thread-123:7"),
    );
    incoming.insert(
        "x-codex-parent-thread-id",
        HeaderValue::from_static("thread-parent"),
    );
    incoming.insert(
        "x-openai-subagent",
        HeaderValue::from_static("collab_spawn"),
    );

    let headers = build_responses_extra_headers(&incoming, FingerprintMode::Normalize);

    assert_eq!(
        headers
            .get("x-codex-window-id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-123:7")
    );
    assert_eq!(
        headers
            .get("x-codex-parent-thread-id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-parent")
    );
    assert_eq!(
        headers
            .get("x-openai-subagent")
            .and_then(|value| value.to_str().ok()),
        Some("collab_spawn")
    );
}

#[test]
fn responses_headers_normalize_missing_client_request_id_from_session_id() {
    let mut incoming = HeaderMap::new();
    incoming.insert("session_id", HeaderValue::from_static("abc"));

    let headers = build_responses_extra_headers(&incoming, FingerprintMode::Normalize);

    assert_eq!(
        headers
            .get("x-client-request-id")
            .and_then(|value| value.to_str().ok()),
        Some("abc")
    );
    assert!(headers.get("user-agent").is_none());
}

#[test]
fn unary_headers_do_not_invent_subagent_without_session_source() {
    let compact_headers = build_unary_extra_headers(
        "responses/compact",
        &HeaderMap::new(),
        FingerprintMode::Normalize,
    );
    let memories_headers = build_unary_extra_headers(
        "memories/trace_summarize",
        &HeaderMap::new(),
        FingerprintMode::Normalize,
    );

    assert!(compact_headers.get("x-openai-subagent").is_none());
    assert!(memories_headers.get("x-openai-subagent").is_none());
}

#[test]
fn normalize_mode_derives_subagent_header_from_review_session_source() {
    let mut incoming = HeaderMap::new();
    incoming.insert(
        SESSION_SOURCE_HEADER,
        HeaderValue::from_static(r#"{"subagent":"review"}"#),
    );

    let headers = build_responses_extra_headers(&incoming, FingerprintMode::Normalize);

    assert_eq!(
        headers
            .get("x-openai-subagent")
            .and_then(|value| value.to_str().ok()),
        Some("review")
    );
}

#[test]
fn normalize_mode_ignores_non_json_session_source_header() {
    let mut incoming = HeaderMap::new();
    incoming.insert(SESSION_SOURCE_HEADER, HeaderValue::from_static("exec"));

    let headers = build_responses_extra_headers(&incoming, FingerprintMode::Normalize);

    assert!(headers.get("x-openai-subagent").is_none());
}

#[test]
fn normalize_mode_derives_thread_spawn_as_collab_spawn() {
    let mut incoming = HeaderMap::new();
    incoming.insert(
        SESSION_SOURCE_HEADER,
        HeaderValue::from_static(
            r#"{"subagent":{"thread_spawn":{"parent_thread_id":"ad7f0408-99b8-4f6e-a46f-bd0eec433370","depth":1,"agent_path":null,"agent_nickname":null,"agent_role":null}}}"#,
        ),
    );

    let headers = build_responses_extra_headers(&incoming, FingerprintMode::Normalize);

    assert_eq!(
        headers
            .get("x-openai-subagent")
            .and_then(|value| value.to_str().ok()),
        Some("collab_spawn")
    );
    assert_eq!(
        headers
            .get("x-codex-parent-thread-id")
            .and_then(|value| value.to_str().ok()),
        Some("ad7f0408-99b8-4f6e-a46f-bd0eec433370")
    );
}

#[test]
fn compact_headers_forward_responses_identity_headers() {
    let mut incoming = HeaderMap::new();
    incoming.insert("session_id", HeaderValue::from_static("thread-123"));
    incoming.insert(
        "x-codex-window-id",
        HeaderValue::from_static("thread-123:1"),
    );
    incoming.insert(
        "x-codex-parent-thread-id",
        HeaderValue::from_static("thread-parent"),
    );
    incoming.insert("x-openai-subagent", HeaderValue::from_static("compact"));

    let headers =
        build_unary_extra_headers("responses/compact", &incoming, FingerprintMode::Normalize);

    assert_eq!(
        headers
            .get("session_id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-123")
    );
    assert_eq!(
        headers
            .get("x-codex-window-id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-123:1")
    );
    assert_eq!(
        headers
            .get("x-codex-parent-thread-id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-parent")
    );
    assert_eq!(
        headers
            .get("x-openai-subagent")
            .and_then(|value| value.to_str().ok()),
        Some("compact")
    );
}

#[test]
fn responses_stream_request_matches_codex_transport_expectations() {
    let mut req = Request {
        method: Method::POST,
        url: "https://chatgpt.com/backend-api/codex/responses".to_string(),
        headers: HeaderMap::new(),
        body: None,
        compression: RequestCompression::None,
        timeout: None,
    };

    configure_responses_stream_request(&mut req);

    assert_eq!(
        req.headers
            .get(ACCEPT)
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(req.compression, RequestCompression::Zstd);
    assert_eq!(req.timeout, None);
}

#[test]
fn websocket_headers_include_beta_marker() {
    let headers =
        build_responses_websocket_headers(&HeaderMap::new(), FingerprintMode::Normalize, "0.117.0");

    assert_eq!(
        headers
            .get(OPENAI_BETA_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE)
    );
    assert_eq!(
        headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some(build_codex_user_agent("0.117.0").as_str())
    );
}

#[test]
fn websocket_headers_forward_responses_identity_headers() {
    let mut incoming = HeaderMap::new();
    incoming.insert(
        "x-codex-window-id",
        HeaderValue::from_static("thread-123:2"),
    );
    incoming.insert(
        "x-codex-parent-thread-id",
        HeaderValue::from_static("thread-parent"),
    );
    incoming.insert("x-openai-subagent", HeaderValue::from_static("review"));

    let headers =
        build_responses_websocket_headers(&incoming, FingerprintMode::Normalize, "0.117.0");

    assert_eq!(
        headers
            .get("x-codex-window-id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-123:2")
    );
    assert_eq!(
        headers
            .get("x-codex-parent-thread-id")
            .and_then(|value| value.to_str().ok()),
        Some("thread-parent")
    );
    assert_eq!(
        headers
            .get("x-openai-subagent")
            .and_then(|value| value.to_str().ok()),
        Some("review")
    );
}

#[test]
fn sanitize_response_headers_normalizes_rate_limit_usage_percent_headers() {
    let mut headers = HeaderMap::new();
    headers.insert(
        "x-codex-primary-used-percent",
        HeaderValue::from_static("95.0"),
    );
    headers.insert(
        "x-codex-other-secondary-used-percent",
        HeaderValue::from_static("87.5"),
    );
    headers.insert(
        "x-codex-primary-window-minutes",
        HeaderValue::from_static("15"),
    );

    let sanitized = sanitize_response_headers(&headers);

    assert_eq!(
        sanitized
            .get("x-codex-primary-used-percent")
            .and_then(|value| value.to_str().ok()),
        Some("0.0")
    );
    assert_eq!(
        sanitized
            .get("x-codex-other-secondary-used-percent")
            .and_then(|value| value.to_str().ok()),
        Some("0.0")
    );
    assert_eq!(
        sanitized
            .get("x-codex-primary-window-minutes")
            .and_then(|value| value.to_str().ok()),
        Some("15")
    );
}

#[test]
fn sanitize_response_headers_removes_hop_by_hop_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", HeaderValue::from_static("keep-alive"));
    headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
    headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
    headers.insert("upgrade", HeaderValue::from_static("websocket"));
    headers.insert("te", HeaderValue::from_static("trailers"));
    headers.insert("trailer", HeaderValue::from_static("expires"));
    headers.insert(
        "proxy-authenticate",
        HeaderValue::from_static("Basic realm=\"upstream\""),
    );
    headers.insert(
        "proxy-authorization",
        HeaderValue::from_static("Basic abc123"),
    );
    headers.insert("content-type", HeaderValue::from_static("application/json"));

    let sanitized = sanitize_response_headers(&headers);

    assert!(sanitized.get("connection").is_none());
    assert!(sanitized.get("keep-alive").is_none());
    assert!(sanitized.get("transfer-encoding").is_none());
    assert!(sanitized.get("upgrade").is_none());
    assert!(sanitized.get("te").is_none());
    assert!(sanitized.get("trailer").is_none());
    assert!(sanitized.get("proxy-authenticate").is_none());
    assert!(sanitized.get("proxy-authorization").is_none());
    assert_eq!(
        sanitized
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
}

#[test]
fn sanitize_response_headers_removes_connection_extension_headers() {
    let mut headers = HeaderMap::new();
    headers.insert("connection", HeaderValue::from_static("x-keep, x-next"));
    headers.insert("x-keep", HeaderValue::from_static("value-a"));
    headers.insert("x-next", HeaderValue::from_static("value-b"));
    headers.insert("x-safe", HeaderValue::from_static("value-c"));

    let sanitized = sanitize_response_headers(&headers);

    assert!(sanitized.get("connection").is_none());
    assert!(sanitized.get("x-keep").is_none());
    assert!(sanitized.get("x-next").is_none());
    assert_eq!(
        sanitized
            .get("x-safe")
            .and_then(|value| value.to_str().ok()),
        Some("value-c")
    );
}

#[test]
fn apply_http_request_timeout_sets_timeout() {
    let mut req = Request {
        method: Method::GET,
        url: "https://chatgpt.com/backend-api/codex/models".to_string(),
        headers: HeaderMap::new(),
        body: None,
        compression: RequestCompression::None,
        timeout: None,
    };

    apply_http_request_timeout(&mut req, Some(Duration::from_secs(600)));

    assert_eq!(req.timeout, Some(Duration::from_secs(600)));
}

#[test]
fn upstream_client_uses_request_timeout_for_refresh_and_unary_http_requests() {
    let client = UpstreamClient::new(
        "https://chatgpt.com/backend-api/codex".to_string(),
        "0.118.0".to_string(),
        FingerprintMode::Normalize,
        321,
    )
    .expect("client builds");

    assert_eq!(client.unary_request_timeout, Some(Duration::from_secs(321)));
}

#[test]
fn upstream_client_does_not_set_total_timeout_for_stream_requests() {
    let client = UpstreamClient::new(
        "https://chatgpt.com/backend-api/codex".to_string(),
        "0.118.0".to_string(),
        FingerprintMode::Normalize,
        321,
    )
    .expect("client builds");

    assert_eq!(client.stream_request_timeout, None);
}

#[test]
fn parse_retry_after_header_supports_http_date() {
    let future = (chrono::Utc::now() + chrono::Duration::seconds(75)).to_rfc2822();
    let mut headers = HeaderMap::new();
    headers.insert(
        "retry-after",
        HeaderValue::from_str(&future).expect("valid header"),
    );

    let parsed = parse_retry_after_header(&headers).expect("parsed retry-after");

    assert!(parsed.as_secs() <= 75);
    assert!(parsed.as_secs() >= 74);
}
