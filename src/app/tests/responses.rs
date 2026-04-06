use super::*;
use crate::classifier::FailureClass;
use crate::failover::FailoverFailure;
use crate::responses::responses_pre_stream_failure_response;
use crate::responses::{ManagedResponseStream, ResponsesSseState};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use codex_api::ApiError;
use futures::stream;

#[tokio::test]
async fn managed_response_stream_marks_failure_on_clean_eof_without_completed() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Ok(Bytes::from_static(b"ok"))]).boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let first = stream.next().await.expect("first chunk");
    assert_eq!(first.expect("chunk ok"), Bytes::from_static(b"ok"));
    let terminal = stream
        .next()
        .await
        .expect("terminal chunk")
        .expect("chunk ok");
    let terminal_text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(terminal_text.contains("event: response.failed"));
    assert!(terminal_text.contains("stream closed before response.completed"));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert!(view.last_success_at.is_none());
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_marks_success_after_completed_event() {
    let (state, account_id) = seeded_state().await;
    let completed = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
    );
    let stream = stream::iter(vec![Ok(completed)]).boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        String::from_utf8(chunk.to_vec())
            .expect("utf8")
            .contains("response.completed")
    );
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert!(view.last_success_at.is_some());
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_none());
}

#[tokio::test]
async fn managed_response_stream_completed_then_transport_error_does_not_emit_extra_failure() {
    let (state, account_id) = seeded_state().await;
    let completed = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
    );
    let stream = stream::iter(vec![
        Ok(completed),
        Err(codex_client::TransportError::Network("boom".to_string())),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        String::from_utf8(chunk.to_vec())
            .expect("utf8")
            .contains("response.completed")
    );
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert!(view.last_success_at.is_some());
    assert!(view.last_error.is_none());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_marks_failure_after_incomplete_event_without_extra_synthetic() {
    let (state, account_id) = seeded_state().await;
    let incomplete = Bytes::from_static(
        b"event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"sequence_number\":1,\"response\":{\"id\":\"resp_incomplete\",\"object\":\"response\",\"created_at\":1,\"status\":\"incomplete\",\"background\":false,\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(incomplete)]).boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        String::from_utf8(chunk.to_vec())
            .expect("utf8")
            .contains("response.incomplete")
    );
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert!(view.last_success_at.is_none());
    assert_eq!(
        view.last_error.as_deref(),
        Some("Incomplete response returned, reason: max_output_tokens")
    );
    assert!(view.blocked_reason.is_none());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_incomplete_then_transport_error_does_not_emit_extra_failure() {
    let (state, account_id) = seeded_state().await;
    let incomplete = Bytes::from_static(
        b"event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\"sequence_number\":1,\"response\":{\"id\":\"resp_incomplete\",\"object\":\"response\",\"created_at\":1,\"status\":\"incomplete\",\"background\":false,\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
    );
    let stream = stream::iter(vec![
        Ok(incomplete),
        Err(codex_client::TransportError::Network("boom".to_string())),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        String::from_utf8(chunk.to_vec())
            .expect("utf8")
            .contains("response.incomplete")
    );
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.last_error.as_deref(),
        Some("Incomplete response returned, reason: max_output_tokens")
    );
    assert!(view.last_success_at.is_none());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_marks_failure_on_upstream_error() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Err(codex_client::TransportError::Network(
        "boom".to_string(),
    ))])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let result = stream.next().await.expect("first item");
    let chunk = result.expect("synthetic chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("\"code\":\"network_error\""));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.routing_state,
        crate::accounts::RoutingState::TemporarilyUnavailable
    );
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::TemporarilyUnavailable)
    );
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_reuses_response_id_and_advances_sequence_number() {
    let (state, account_id) = seeded_state().await;
    let first = Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":7,\"response\":{\"id\":\"resp_existing\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n\n",
    );
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: None,
        body: Some(
            "{\"error\":{\"message\":\"Rate limit reached for gpt-5.4. Please try again in 11.5s.\",\"code\":\"rate_limit_exceeded\"}}"
                .to_string(),
        ),
    };
    let stream = stream::iter(vec![Ok(first), Err(error)]).boxed();
    let mut stream = ManagedResponseStream::new(state, account_id, stream);

    let _ = stream.next().await.expect("upstream event").expect("ok");
    let chunk = stream.next().await.expect("synthetic event").expect("ok");
    let payload = String::from_utf8(chunk.to_vec()).expect("utf8");

    assert!(payload.contains("\"sequence_number\":8"));
    assert!(payload.contains("\"id\":\"resp_existing\""));
    assert!(payload.contains("\"code\":\"rate_limit_exceeded\""));
}

#[tokio::test]
async fn synthetic_failed_event_is_understood_by_codex_parser() {
    let sse_state = ResponsesSseState::with_checkpoint(Some(2), Some("resp_test".to_string()));
    let chunk = sse_state
        .synthetic_failed_event(&codex_client::TransportError::Http {
            status: StatusCode::TOO_MANY_REQUESTS,
            url: None,
            headers: None,
            body: Some(
                "{\"error\":{\"message\":\"Rate limit reached for gpt-5.4. Please try again in 11.054s.\",\"code\":\"rate_limit_exceeded\"}}"
                    .to_string(),
            ),
        })
        .expect("synthetic chunk");

    let results = collect_codex_sse_results(vec![Ok(chunk)]).await;

    assert_eq!(results.len(), 1);
    match &results[0] {
        Err(ApiError::Retryable { message, delay }) => {
            assert!(message.contains("Rate limit reached"));
            assert!(delay.is_some());
        }
        other => panic!("unexpected parser result: {other:?}"),
    }
}

#[tokio::test]
async fn synthetic_context_window_event_is_understood_by_codex_parser() {
    let chunk = ResponsesSseState::default()
        .synthetic_failed_event(&codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: None,
            body: Some(
                "{\"error\":{\"message\":\"Your input exceeds the context window of this model. Please adjust your input and try again.\",\"code\":\"context_length_exceeded\"}}"
                    .to_string(),
            ),
        })
        .expect("synthetic chunk");

    let results = collect_codex_sse_results(vec![Ok(chunk)]).await;

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], Err(ApiError::ContextWindowExceeded)));
}

#[tokio::test]
async fn synthetic_quota_event_is_understood_by_codex_parser() {
    let chunk = ResponsesSseState::default()
        .synthetic_failed_event(&codex_client::TransportError::Http {
            status: StatusCode::FORBIDDEN,
            url: None,
            headers: None,
            body: Some(
                "{\"error\":{\"message\":\"You exceeded your current quota, please check your plan and billing details.\",\"code\":\"insufficient_quota\"}}"
                    .to_string(),
            ),
        })
        .expect("synthetic chunk");

    let results = collect_codex_sse_results(vec![Ok(chunk)]).await;

    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], Err(ApiError::QuotaExceeded)));
}

#[tokio::test]
async fn responses_pre_stream_http_429_returns_synthetic_sse() {
    let response = responses_pre_stream_failure_response(&FailoverFailure::Transport(
        codex_client::TransportError::Http {
            status: StatusCode::TOO_MANY_REQUESTS,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert("retry-after", HeaderValue::from_static("11"));
                headers.insert(
                    "x-codex-primary-used-percent",
                    HeaderValue::from_static("95.0"),
                );
                headers
            }),
            body: Some(
                "{\"error\":{\"message\":\"Rate limit reached for gpt-5.4. Please try again in 11s.\",\"code\":\"rate_limit_exceeded\"}}"
                    .to_string(),
            ),
        },
    ));

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert!(headers.get("connection").is_none());
    assert!(headers.get("x-codex-primary-used-percent").is_none());
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));

    let results = collect_codex_sse_results(vec![Ok(body)]).await;
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], Err(ApiError::Retryable { .. })));
}

#[tokio::test]
async fn synthetic_failed_event_preserves_resets_in_seconds_from_http_body() {
    let chunk = ResponsesSseState::default()
        .synthetic_failed_event(&codex_client::TransportError::Http {
            status: StatusCode::BAD_GATEWAY,
            url: None,
            headers: None,
            body: Some(
                r#"{"error":{"message":"The usage limit has been reached","type":"usage_limit_reached","resets_in_seconds":77}}"#
                    .to_string(),
            ),
        })
        .expect("synthetic chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");

    assert!(text.contains(r#""resets_in_seconds":77"#));
}

#[tokio::test]
async fn synthetic_failed_event_preserves_both_reset_fields_from_http_body() {
    let chunk = ResponsesSseState::default()
        .synthetic_failed_event(&codex_client::TransportError::Http {
            status: StatusCode::BAD_GATEWAY,
            url: None,
            headers: None,
            body: Some(
                r#"{"error":{"message":"The usage limit has been reached","type":"usage_limit_reached","resets_at":1775973729,"resets_in_seconds":77}}"#
                    .to_string(),
            ),
        })
        .expect("synthetic chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");

    assert!(text.contains(r#""resets_at":1775973729"#));
    assert!(text.contains(r#""resets_in_seconds":77"#));
}

#[tokio::test]
async fn responses_pre_stream_refresh_quota_failure_returns_synthetic_sse() {
    let response = responses_pre_stream_failure_response(&FailoverFailure::Refresh(
        crate::upstream::RefreshFailure {
            status: StatusCode::FORBIDDEN,
            body: "{\"error\":{\"message\":\"You exceeded your current quota, please check your plan and billing details.\",\"code\":\"insufficient_quota\"}}".to_string(),
            class: FailureClass::QuotaExhausted,
            retry_after: None,
        },
    ));

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let results = collect_codex_sse_results(vec![Ok(body)]).await;
    assert_eq!(results.len(), 1);
    assert!(matches!(results[0], Err(ApiError::QuotaExceeded)));
}

#[tokio::test]
async fn responses_pre_stream_non_json_500_returns_synthetic_sse() {
    let response = responses_pre_stream_failure_response(&FailoverFailure::Transport(
        codex_client::TransportError::Http {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            url: None,
            headers: None,
            body: Some("upstream exploded".to_string()),
        },
    ));

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("\"code\":\"internal_server_error\""));
    assert!(text.contains("\"message\":\"Upstream request failed with status 500.\""));
}

#[tokio::test]
async fn synthetic_bodyless_403_event_is_not_understood_as_quota() {
    let chunk = ResponsesSseState::default()
        .synthetic_failed_event(&codex_client::TransportError::Http {
            status: StatusCode::FORBIDDEN,
            url: None,
            headers: None,
            body: None,
        })
        .expect("synthetic chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("\"code\":\"forbidden\""));

    let results = collect_codex_sse_results(vec![Ok(chunk)]).await;

    assert_eq!(results.len(), 1);
    assert!(!matches!(results[0], Err(ApiError::QuotaExceeded)));
}

#[tokio::test]
async fn managed_response_stream_release_on_drop_does_not_mark_success() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::pending::<Result<Bytes, codex_client::TransportError>>().boxed();
    let stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);
    drop(stream);
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert!(view.last_success_at.is_none());
    assert!(view.last_error.is_none());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_rate_limit_failure_settles_before_eof() {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 11.5s.\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(failed)]).boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        String::from_utf8(chunk.to_vec())
            .expect("utf8")
            .contains("response.failed")
    );
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::RateLimited)
    );
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_failed_then_transport_error_does_not_emit_extra_failure() {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 11.5s.\"}}}\n\n",
    );
    let stream = stream::iter(vec![
        Ok(failed),
        Err(codex_client::TransportError::Network("boom".to_string())),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        String::from_utf8(chunk.to_vec())
            .expect("utf8")
            .contains("response.failed")
    );
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::RateLimited)
    );
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_request_rejected_transport_records_last_error_without_blocking() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Err(codex_client::TransportError::Http {
        status: StatusCode::BAD_REQUEST,
        url: None,
        headers: None,
        body: Some(r#"{"error":{"code":"invalid_prompt","message":"bad prompt"}}"#.to_string()),
    })])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let result = stream.next().await.expect("first item");
    let chunk = result.expect("synthetic chunk");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("\"code\":\"invalid_prompt\""));
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.last_error.as_deref(),
        Some(
            "RequestRejected: status=400 Bad Request body={\"error\":{\"code\":\"invalid_prompt\",\"message\":\"bad prompt\"}}"
        )
    );
    assert!(view.blocked_reason.is_none());
    assert!(view.blocked_until.is_none());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_http_usage_limit_sets_block_from_body_resets() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Err(codex_client::TransportError::Http {
        status: StatusCode::BAD_GATEWAY,
        url: None,
        headers: None,
        body: Some(
            r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#
                .to_string(),
        ),
    })])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let _ = stream.next().await.expect("synthetic event").expect("ok");
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::QuotaExhausted)
    );
    assert_eq!(
        view.blocked_source,
        Some(crate::accounts::BlockedSource::UpstreamRetryAfter)
    );
    assert!(view.blocked_until.is_some());
    assert!(
        view.blocked_until.expect("blocked until")
            > chrono::Utc::now() + chrono::Duration::seconds(70)
    );
}

#[tokio::test]
async fn managed_response_stream_http_websocket_connection_limit_is_not_quota() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Err(codex_client::TransportError::Http {
        status: StatusCode::BAD_REQUEST,
        url: None,
        headers: None,
        body: Some(
            r#"{"error":{"type":"invalid_request_error","code":"websocket_connection_limit_reached","message":"Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue."}}"#
                .to_string(),
        ),
    })])
    .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let _ = stream.next().await.expect("synthetic event").expect("ok");
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::TemporarilyUnavailable)
    );
    assert_ne!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::QuotaExhausted)
    );
}

#[tokio::test]
async fn managed_response_stream_quota_failure_settles_before_eof() {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_quota\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"insufficient_quota\",\"message\":\"You exceeded your current quota, please check your plan and billing details.\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(failed)]).boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let _ = stream.next().await.expect("first chunk").expect("chunk ok");
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::QuotaExhausted)
    );
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_completed_then_drop_records_success() {
    let (state, account_id) = seeded_state().await;
    let completed = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
    );
    let stream = stream::iter(vec![Ok(completed)])
        .chain(stream::pending::<Result<Bytes, codex_client::TransportError>>())
        .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let _ = stream.next().await.expect("first chunk").expect("chunk ok");
    drop(stream);
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert!(view.last_success_at.is_some());
    assert!(view.last_error.is_none());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_failed_then_drop_preserves_failure() {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 8s.\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(failed)])
        .chain(stream::pending::<Result<Bytes, codex_client::TransportError>>())
        .boxed();
    let mut stream = ManagedResponseStream::new(state.clone(), account_id.clone(), stream);

    let _ = stream.next().await.expect("first chunk").expect("chunk ok");
    drop(stream);
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::RateLimited)
    );
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}
