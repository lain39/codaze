use super::*;
use crate::classifier::FailureClass;
use crate::failover::FailoverFailure;
use crate::models::ResponseShape;
use crate::responses::responses_pre_stream_failure_response;
use crate::responses::{ManagedResponseStream, ResponsesSseState};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use bytes::Bytes;
use codex_api::ApiError;
use futures::stream;
use std::time::Duration;

#[tokio::test]
async fn managed_response_stream_marks_failure_on_clean_eof_without_completed() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Ok(Bytes::from_static(b"ok"))]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
async fn managed_response_stream_marks_success_after_completed_event_without_trailing_separator() {
    let (state, account_id) = seeded_state().await;
    let completed = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n",
    );
    let stream = stream::iter(vec![Ok(completed)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("response.completed"));
    assert!(!text.contains("stream closed before response.completed"));
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
async fn managed_response_stream_eof_chunk_drop_still_marks_failure() {
    let (state, account_id) = seeded_state().await;
    let created = Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_progress\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n",
    );
    let stream = stream::iter(vec![Ok(created)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("response.created"));
    drop(stream);
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
async fn managed_response_stream_keeps_failed_event_without_trailing_separator() {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_fail\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 8s.\"}}}\n",
    );
    let stream = stream::iter(vec![Ok(failed)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("response.failed"));
    assert!(!text.contains("stream closed before response.completed"));
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
        Some("Rate limit reached for gpt-5.4. Please try again in 8s.")
    );
    assert_eq!(view.in_flight_requests, 0);
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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
async fn managed_response_stream_discards_partial_sse_fragment_before_transport_failure() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![
        Ok(Bytes::from_static(b"data: partial")),
        Err(codex_client::TransportError::Network("boom".to_string())),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("\"code\":\"network_error\""));
    assert!(!text.contains("data: partial"));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_discards_split_sse_prefix_before_transport_failure() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![
        Ok(Bytes::from_static(b"dat")),
        Err(codex_client::TransportError::Network("boom".to_string())),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(text.starts_with("event: response.failed"));
    assert!(text.contains("\"code\":\"network_error\""));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_discards_partial_sse_fragment_before_clean_eof() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Ok(Bytes::from_static(b"data: partial"))]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("stream closed before response.completed"));
    assert!(!text.contains("data: partial"));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_discards_split_event_prefix_before_clean_eof() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Ok(Bytes::from_static(b"eve"))]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(text.starts_with("event: error"));
    assert!(text.contains("stream closed before response.completed"));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.in_flight_requests, 0);
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_reassembles_split_event_prefix_before_completed() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![
        Ok(Bytes::from_static(b"eve")),
        Ok(Bytes::from_static(
            b"nt: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
        )),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: response.completed"));
    assert!(!text.contains("event: response.failed"));
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
async fn managed_response_stream_reassembles_split_data_prefix_before_failed() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![
        Ok(Bytes::from_static(b"da")),
        Ok(Bytes::from_static(
            b"ta: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 8s.\"}}}\n\n",
        )),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("\"type\":\"response.failed\""));
    assert!(!text.starts_with("da"));
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
    assert!(view.last_error.is_some());
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_openai_shape_emits_error_event_on_upstream_error() {
    let (state, account_id) = seeded_state().await;
    let first = Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":4,\"response\":{\"id\":\"resp_existing\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n\n",
    );
    let stream = stream::iter(vec![
        Ok(first),
        Err(codex_client::TransportError::Network("boom".to_string())),
    ])
    .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let first = stream.next().await.expect("first item").expect("chunk ok");
    assert!(
        String::from_utf8(first.to_vec())
            .expect("utf8")
            .contains("response.created")
    );
    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"type\":\"error\""));
    assert!(text.contains("\"code\":\"network_error\""));
    assert!(text.contains("\"sequence_number\":5"));
    assert!(!text.contains("response.failed"));
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
        Some(crate::accounts::BlockedReason::TemporarilyUnavailable)
    );
    assert_eq!(view.in_flight_requests, 0);
}

#[tokio::test]
async fn managed_response_stream_openai_shape_emits_error_event_on_clean_eof_without_completed() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::iter(vec![Ok(Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":0,\"response\":{\"id\":\"resp_existing\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n\n",
    ))])
    .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let _ = stream.next().await.expect("first item").expect("chunk ok");
    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"message\":\"stream closed before response.completed\""));
    assert!(!text.contains("response.failed"));
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
async fn managed_response_stream_openai_upstream_error_event_is_terminal_without_extra_synthetic() {
    let (state, account_id) = seeded_state().await;
    let errored = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached\",\"sequence_number\":7}\n\n",
    );
    let stream = stream::iter(vec![Ok(errored)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let chunk = stream.next().await.expect("first item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"sequence_number\":7"));
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
    assert!(view.last_error.is_some());
}

#[tokio::test]
async fn managed_response_stream_rewrites_generic_forbidden_response_failed_to_gateway_unavailable()
{
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_forbidden\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"forbidden\",\"message\":\"forbidden\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(failed)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("\"code\":\"forbidden\""));
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
        Some(crate::accounts::BlockedReason::TemporarilyUnavailable)
    );
    assert_eq!(view.last_error.as_deref(), Some("forbidden"));
}

#[tokio::test]
async fn managed_response_stream_rewrites_invalid_api_key_error_to_gateway_unavailable() {
    let (state, account_id) = seeded_state().await;
    let errored = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"code\":\"invalid_api_key\",\"message\":\"bad key\",\"sequence_number\":7}\n\n",
    );
    let stream = stream::iter(vec![Ok(errored)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let chunk = stream.next().await.expect("first item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"sequence_number\":7"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("\"code\":\"invalid_api_key\""));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.last_error.as_deref(), Some("bad key"));
}

#[tokio::test]
async fn managed_response_stream_rewrites_authentication_error_typed_event_to_gateway_unavailable()
{
    let (state, account_id) = seeded_state().await;
    let errored = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"authentication_error\",\"code\":\"invalid_api_key\",\"message\":\"bad key\",\"sequence_number\":7}\n\n",
    );
    let stream = stream::iter(vec![Ok(errored)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let chunk = stream.next().await.expect("first item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"sequence_number\":7"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("\"code\":\"invalid_api_key\""));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.last_error.as_deref(), Some("bad key"));
}

#[tokio::test]
async fn managed_response_stream_rewrites_wrapped_authentication_error_event_to_gateway_unavailable()
 {
    let (state, account_id) = seeded_state().await;
    let errored = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"status\":401,\"error\":{\"type\":\"authentication_error\",\"code\":\"invalid_api_key\",\"message\":\"bad key\"}}\n\n",
    );
    let stream = stream::iter(vec![Ok(errored)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let chunk = stream.next().await.expect("first item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("\"code\":\"invalid_api_key\""));
    assert!(stream.next().await.is_none());
    yield_for_settlement().await;

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.last_error.as_deref(), Some("bad key"));
}

#[tokio::test]
async fn managed_response_stream_rewrites_permission_error_typed_response_failed_to_gateway_unavailable()
 {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_forbidden_typed\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"type\":\"permission_error\",\"code\":\"forbidden\",\"message\":\"forbidden\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(failed)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let chunk = stream.next().await.expect("first item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("\"code\":\"forbidden\""));
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
        Some(crate::accounts::BlockedReason::TemporarilyUnavailable)
    );
    assert_eq!(view.last_error.as_deref(), Some("forbidden"));
}

#[tokio::test]
async fn managed_response_stream_wrapped_gateway_unavailable_error_advances_sequence_number() {
    let (state, account_id) = seeded_state().await;
    let first = Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":7,\"response\":{\"id\":\"resp_existing\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n\n",
    );
    let wrapped = Bytes::from_static(
        b"event: error\ndata: {\"type\":\"error\",\"status\":401,\"error\":{\"type\":\"authentication_error\",\"code\":\"invalid_api_key\",\"message\":\"bad key\"}}\n\n",
    );
    let stream = stream::iter(vec![Ok(first), Ok(wrapped)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::OpenAi,
    );

    let _ = stream.next().await.expect("first item").expect("chunk ok");
    let chunk = stream.next().await.expect("second item").expect("chunk ok");
    let text = String::from_utf8(chunk.to_vec()).expect("utf8");
    assert!(text.contains("event: error"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("\"sequence_number\":8"));
    assert!(stream.next().await.is_none());
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
        headers: Some({
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_static("11"));
            headers
        }),
        body: Some(
            "{\"error\":{\"message\":\"The usage limit has been reached\",\"type\":\"usage_limit_reached\",\"resets_in_seconds\":77}}"
                .to_string(),
        ),
    };
    let stream = stream::iter(vec![Ok(first), Err(error)]).boxed();
    let mut stream = ManagedResponseStream::new(state, account_id, stream, ResponseShape::Codex);

    let _ = stream.next().await.expect("upstream event").expect("ok");
    let chunk = stream.next().await.expect("synthetic event").expect("ok");
    let payload = String::from_utf8(chunk.to_vec()).expect("utf8");

    assert!(payload.contains("\"sequence_number\":8"));
    assert!(payload.contains("\"id\":\"resp_existing\""));
    assert!(payload.contains("\"code\":\"server_is_overloaded\""));
    assert!(payload.contains("No account available right now. Try again later."));
    assert!(!payload.contains("try again in"));
    assert!(!payload.contains("\"resets_at\""));
    assert!(!payload.contains("\"resets_in_seconds\""));
}

#[tokio::test]
async fn managed_response_stream_openai_shape_transport_gateway_unavailable_strips_retry_metadata()
{
    let (state, account_id) = seeded_state().await;
    let first = Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":4,\"response\":{\"id\":\"resp_existing\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n\n",
    );
    let error = codex_client::TransportError::Http {
        status: StatusCode::TOO_MANY_REQUESTS,
        url: None,
        headers: Some({
            let mut headers = HeaderMap::new();
            headers.insert("retry-after", HeaderValue::from_static("11"));
            headers
        }),
        body: Some(
            r#"{"error":{"message":"The usage limit has been reached","type":"usage_limit_reached","resets_at":1775973729,"resets_in_seconds":77}}"#
                .to_string(),
        ),
    };
    let stream = stream::iter(vec![Ok(first), Err(error)]).boxed();
    let mut stream = ManagedResponseStream::new(state, account_id, stream, ResponseShape::OpenAi);

    let _ = stream.next().await.expect("first item").expect("chunk ok");
    let terminal = stream
        .next()
        .await
        .expect("terminal item")
        .expect("chunk ok");
    let text = String::from_utf8(terminal.to_vec()).expect("utf8");

    assert!(text.contains("event: error"));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("try again in"));
    assert!(!text.contains("\"resets_at\""));
    assert!(!text.contains("\"resets_in_seconds\""));
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
    ), ResponseShape::Codex);

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
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
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
    ), ResponseShape::Codex);

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
}

#[tokio::test]
async fn responses_pre_stream_non_json_500_returns_synthetic_sse() {
    let response = responses_pre_stream_failure_response(
        &FailoverFailure::Transport(codex_client::TransportError::Http {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            url: None,
            headers: None,
            body: Some("upstream exploded".to_string()),
        }),
        ResponseShape::Codex,
    );

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
}

#[tokio::test]
async fn responses_pre_stream_http_429_returns_json_error_for_non_codex_clients() {
    let response = responses_pre_stream_failure_response(
        &FailoverFailure::Transport(codex_client::TransportError::Http {
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
        }),
        ResponseShape::OpenAi,
    );

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert!(headers.get("connection").is_none());
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("response.failed"));
}

#[tokio::test]
async fn responses_pre_stream_plain_text_http_failure_returns_openai_json_error_for_non_codex_clients()
 {
    let response = responses_pre_stream_failure_response(
        &FailoverFailure::Transport(codex_client::TransportError::Http {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            url: None,
            headers: Some({
                let mut headers = HeaderMap::new();
                headers.insert(
                    "content-type",
                    HeaderValue::from_static("text/plain; charset=utf-8"),
                );
                headers
            }),
            body: Some("upstream exploded".to_string()),
        }),
        ResponseShape::OpenAi,
    );

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("No account available right now. Try again later."));
    assert!(text.contains("\"type\":\"server_error\""));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("\"param\":null"));
}

#[tokio::test]
async fn responses_pre_stream_plain_text_unauthorized_uses_authentication_error_for_non_codex_clients()
 {
    let response = responses_pre_stream_failure_response(
        &FailoverFailure::Transport(codex_client::TransportError::Http {
            status: StatusCode::UNAUTHORIZED,
            url: None,
            headers: None,
            body: Some("no auth".to_string()),
        }),
        ResponseShape::OpenAi,
    );

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("\"type\":\"server_error\""));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
}

#[tokio::test]
async fn responses_pre_stream_plain_text_forbidden_uses_permission_error_for_non_codex_clients() {
    let response = responses_pre_stream_failure_response(
        &FailoverFailure::Transport(codex_client::TransportError::Http {
            status: StatusCode::FORBIDDEN,
            url: None,
            headers: None,
            body: Some("forbidden".to_string()),
        }),
        ResponseShape::OpenAi,
    );

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("\"type\":\"server_error\""));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
}

#[tokio::test]
async fn responses_pre_stream_pool_block_returns_json_error_for_non_codex_clients() {
    let response = responses_pre_stream_failure_response(
        &FailoverFailure::PoolBlocked(crate::accounts::PoolBlockSummary {
            blocked_reason: crate::accounts::BlockedReason::QuotaExhausted,
            blocked_until: None,
            retry_after: None,
        }),
        ResponseShape::OpenAi,
    );

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("No account available right now. Try again later."));
    assert!(text.contains("\"code\":\"server_is_overloaded\""));
    assert!(text.contains("\"type\":\"server_error\""));
    assert!(!text.contains("response.failed"));
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
    assert!(text.contains("\"code\":\"http_error\""));
    assert!(!text.contains("\"code\":\"insufficient_quota\""));

    let results = collect_codex_sse_results(vec![Ok(chunk)]).await;

    assert_eq!(results.len(), 1);
    assert!(!matches!(results[0], Err(ApiError::QuotaExceeded)));
}

#[tokio::test]
async fn managed_response_stream_release_on_drop_does_not_mark_success() {
    let (state, account_id) = seeded_state().await;
    let stream = stream::pending::<Result<Bytes, codex_client::TransportError>>().boxed();
    let stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );
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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

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
async fn managed_response_stream_completed_closes_without_waiting_for_eof() {
    let (state, account_id) = seeded_state().await;
    let completed = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
    );
    let stream = stream::iter(vec![Ok(completed)])
        .chain(stream::pending::<Result<Bytes, codex_client::TransportError>>())
        .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let _ = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("stream should finish promptly")
            .is_none()
    );
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
async fn managed_response_stream_same_chunk_locks_first_completed_terminal_event() {
    let (state, account_id) = seeded_state().await;
    let combined = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\nevent: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":2,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"should be ignored\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(combined)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let first = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(first.to_vec()).expect("utf8");
    assert!(text.contains("response.completed"));
    assert!(!text.contains("response.failed"));
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
async fn managed_response_stream_same_chunk_emits_multiple_nonterminal_events_in_order() {
    let (state, account_id) = seeded_state().await;
    let combined = Bytes::from_static(
        b"event: response.created\ndata: {\"type\":\"response.created\",\"sequence_number\":1,\"response\":{\"id\":\"resp_progress\",\"object\":\"response\",\"created_at\":1,\"status\":\"in_progress\",\"background\":false,\"error\":null}}\n\nevent: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"sequence_number\":2,\"delta\":\"OK\"}\n\n",
    );
    let completed = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":3,\"response\":{\"id\":\"resp_progress\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
    );
    let stream = stream::iter(vec![Ok(combined), Ok(completed)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let first = stream.next().await.expect("first chunk").expect("chunk ok");
    let first_text = String::from_utf8(first.to_vec()).expect("utf8");
    assert!(first_text.contains("response.created"));

    let second = stream
        .next()
        .await
        .expect("second chunk")
        .expect("chunk ok");
    let second_text = String::from_utf8(second.to_vec()).expect("utf8");
    assert!(second_text.contains("response.output_text.delta"));
    assert!(second_text.contains("\"delta\":\"OK\""));

    let third = stream.next().await.expect("third chunk").expect("chunk ok");
    let third_text = String::from_utf8(third.to_vec()).expect("utf8");
    assert!(third_text.contains("response.completed"));
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
async fn managed_response_stream_same_chunk_drops_trailing_bytes_after_terminal_event() {
    let (state, account_id) = seeded_state().await;
    let combined = Bytes::from_static(
        b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n: trailing comment that must be dropped\n\ngarbage after terminal",
    );
    let stream = stream::iter(vec![Ok(combined)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let first = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(first.to_vec()).expect("utf8");
    assert!(text.contains("response.completed"));
    assert!(!text.contains("trailing comment"));
    assert!(!text.contains("garbage after terminal"));
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
async fn managed_response_stream_failed_closes_without_waiting_for_eof() {
    let (state, account_id) = seeded_state().await;
    let failed = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 8s.\"}}}\n\n",
    );
    let stream = stream::iter(vec![Ok(failed)])
        .chain(stream::pending::<Result<Bytes, codex_client::TransportError>>())
        .boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let _ = stream.next().await.expect("first chunk").expect("chunk ok");
    assert!(
        tokio::time::timeout(Duration::from_millis(50), stream.next())
            .await
            .expect("stream should finish promptly")
            .is_none()
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
async fn managed_response_stream_same_chunk_locks_first_failed_terminal_event() {
    let (state, account_id) = seeded_state().await;
    let combined = Bytes::from_static(
        b"event: response.failed\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"failed\",\"background\":false,\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached for gpt-5.4. Please try again in 8s.\"}}}\n\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":2,\"response\":{\"id\":\"resp_rate_limit\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
    );
    let stream = stream::iter(vec![Ok(combined)]).boxed();
    let mut stream = ManagedResponseStream::new(
        state.clone(),
        account_id.clone(),
        stream,
        ResponseShape::Codex,
    );

    let first = stream.next().await.expect("first chunk").expect("chunk ok");
    let text = String::from_utf8(first.to_vec()).expect("utf8");
    assert!(text.contains("response.failed"));
    assert!(!text.contains("response.completed"));
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
    assert!(view.last_error.is_some());
    assert_eq!(view.in_flight_requests, 0);
}
