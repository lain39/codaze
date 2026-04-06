use super::*;
use crate::classifier::FailureClass;
use crate::responses::{
    PendingWebsocketRequest, PendingWebsocketRetryResult, WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE,
    WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE, WebsocketProxyOutcome,
    classify_websocket_error_text, classify_websocket_upstream_message,
    is_responses_websocket_request_start, normalize_rate_limit_event_payload,
    retry_pending_websocket_request, rewrite_previous_response_not_found_message,
    rewrite_previous_response_not_found_payload, should_passthrough_retryable_websocket_reset,
    upstream_message_commits_request, upstream_message_is_terminal,
};
use axum::http::HeaderMap;
use serde_json::Value;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

#[test]
fn websocket_error_text_classifies_wrapped_rate_limit_error() {
    let reset_at = chrono::Utc::now().timestamp() + 12;
    let outcome = classify_websocket_error_text(
        &format!(
            r#"{{"type":"error","status":429,"headers":{{"retry-after":"1"}},"error":{{"type":"usage_limit_reached","message":"The usage limit has been reached","plan_type":"free","resets_at":{reset_at}}}}}"#
        ),
    )
    .expect("classified");

    let WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    } = outcome
    else {
        panic!("expected failed outcome");
    };
    assert_eq!(failure, FailureClass::QuotaExhausted);
    assert!(details.contains("responses websocket upstream returned error event"));
    assert!(details.contains("status=429"));
    assert!(details.contains("error.type=usage_limit_reached"));
    assert!(details.contains("error.message=The usage limit has been reached"));
    assert!(details.contains("error.resets_at="));
    assert!(retry_after.is_some());
    assert!(retry_after.expect("retry after").as_secs() <= 12);
}

#[test]
fn websocket_error_text_prefers_resets_in_seconds_for_usage_limit_error() {
    let outcome = classify_websocket_error_text(
        r#"{"type":"error","status":429,"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#,
    )
    .expect("classified");

    let WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    } = outcome
    else {
        panic!("expected failed outcome");
    };
    assert_eq!(failure, FailureClass::QuotaExhausted);
    assert_eq!(retry_after, Some(Duration::from_secs(77)));
    assert!(details.contains("error.type=usage_limit_reached"));
    assert!(details.contains("error.resets_in_seconds=77"));
}

#[test]
fn websocket_error_text_uses_retry_after_for_usage_limit_even_when_status_is_502() {
    let outcome = classify_websocket_error_text(
        r#"{"type":"error","status":502,"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":597805}}"#,
    )
    .expect("classified");

    let WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    } = outcome
    else {
        panic!("expected failed outcome");
    };
    assert_eq!(failure, FailureClass::QuotaExhausted);
    assert_eq!(retry_after, Some(Duration::from_secs(597805)));
    assert!(details.contains("status=502"));
    assert!(details.contains("error.type=usage_limit_reached"));
    assert!(details.contains("error.resets_in_seconds=597805"));
}

#[test]
fn websocket_connection_limit_error_is_temporary_failure_not_quota() {
    let outcome = classify_websocket_error_text(
        r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","code":"websocket_connection_limit_reached","message":"Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue."}}"#,
    )
    .expect("classified");

    let WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    } = outcome
    else {
        panic!("expected failed outcome");
    };
    assert_eq!(failure, FailureClass::TemporaryFailure);
    assert_eq!(retry_after, None);
    assert!(details.contains("error.code=websocket_connection_limit_reached"));
}

#[test]
fn rewrite_previous_response_not_found_payload_maps_to_retryable_websocket_error() {
    let rewritten = rewrite_previous_response_not_found_payload(
        r#"{"type":"error","error":{"type":"invalid_request_error","code":"previous_response_not_found","message":"Previous response with id 'resp_123' not found.","param":"previous_response_id"},"status":400}"#,
    )
    .expect("rewritten");
    let json: Value = serde_json::from_str(&rewritten).expect("json");

    assert_eq!(json.get("type").and_then(Value::as_str), Some("error"));
    assert_eq!(json.get("status").and_then(Value::as_u64), Some(400));
    let error = json.get("error").expect("error object");
    assert_eq!(
        error.get("type").and_then(Value::as_str),
        Some("invalid_request_error")
    );
    assert_eq!(
        error.get("code").and_then(Value::as_str),
        Some(WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE)
    );
    assert_eq!(
        error.get("message").and_then(Value::as_str),
        Some(WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE)
    );
}

#[test]
fn rewrite_previous_response_not_found_payload_leaves_other_errors_unchanged() {
    let original = r#"{"type":"error","error":{"type":"invalid_request_error","code":"invalid_prompt","message":"bad"},"status":400}"#;
    assert_eq!(rewrite_previous_response_not_found_payload(original), None);
}

#[test]
fn passthrough_retryable_websocket_reset_only_applies_precommit() {
    let message = rewrite_previous_response_not_found_message(TungsteniteMessage::Text(
        r#"{"type":"error","error":{"type":"invalid_request_error","code":"previous_response_not_found","message":"Previous response with id 'resp_123' not found.","param":"previous_response_id"},"status":400}"#
            .into(),
    ));
    let pending_precommit = Some(PendingWebsocketRequest::default());
    let pending_committed = Some(PendingWebsocketRequest {
        committed: true,
        ..Default::default()
    });

    assert!(should_passthrough_retryable_websocket_reset(
        pending_precommit
            .as_ref()
            .is_some_and(|pending| !pending.committed),
        &message
    ));
    assert!(!should_passthrough_retryable_websocket_reset(
        pending_committed
            .as_ref()
            .is_some_and(|pending| !pending.committed),
        &message
    ));
    assert!(!should_passthrough_retryable_websocket_reset(
        false, &message
    ));
}

#[test]
fn websocket_error_text_classifies_response_failed_payload() {
    let outcome = classify_websocket_error_text(
        r#"{"type":"response.failed","response":{"error":{"code":"insufficient_quota","message":"You exceeded your current quota"}}}"#,
    )
    .expect("classified");

    let WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    } = outcome
    else {
        panic!("expected failed outcome");
    };
    assert_eq!(failure, FailureClass::QuotaExhausted);
    assert_eq!(retry_after, None);
    assert_eq!(
        details,
        "responses websocket upstream returned response.failed: You exceeded your current quota"
    );
}

#[test]
fn websocket_close_none_releases() {
    let outcome = classify_websocket_upstream_message(&TungsteniteMessage::Close(None));
    assert_eq!(outcome, Some(WebsocketProxyOutcome::Released));
}

#[test]
fn websocket_close_normal_code_with_empty_reason_releases() {
    let outcome = classify_websocket_upstream_message(&TungsteniteMessage::Close(Some(
        TungsteniteCloseFrame {
            code: CloseCode::Normal,
            reason: "".into(),
        },
    )));

    assert_eq!(outcome, Some(WebsocketProxyOutcome::Released));
}

#[test]
fn websocket_close_reason_classifies_explicit_rate_limit() {
    let outcome = classify_websocket_upstream_message(&TungsteniteMessage::Close(Some(
        TungsteniteCloseFrame {
            code: CloseCode::Policy,
            reason: "Rate limit reached".into(),
        },
    )));

    assert_eq!(
        outcome,
        Some(WebsocketProxyOutcome::Failed {
            failure: FailureClass::RateLimited,
            retry_after: None,
            details: "responses websocket upstream closed with error: reason=Rate limit reached"
                .to_string(),
        })
    );
}

#[test]
fn websocket_abnormal_close_without_reason_is_temporary_failure() {
    let outcome = classify_websocket_upstream_message(&TungsteniteMessage::Close(Some(
        TungsteniteCloseFrame {
            code: CloseCode::Abnormal,
            reason: "".into(),
        },
    )));

    assert_eq!(
        outcome,
        Some(WebsocketProxyOutcome::Failed {
            failure: FailureClass::TemporaryFailure,
            retry_after: None,
            details: "responses websocket upstream closed with error: code=1006 reason="
                .to_string(),
        })
    );
}

#[test]
fn response_create_marks_websocket_request_start() {
    let message =
        TungsteniteMessage::Text(r#"{"type":"response.create","model":"gpt-5.4"}"#.into());
    assert!(is_responses_websocket_request_start(&message));
}

#[test]
fn codex_rate_limits_does_not_commit_buffered_websocket_request() {
    let message = TungsteniteMessage::Text(r#"{"type":"codex.rate_limits"}"#.into());
    assert!(!upstream_message_commits_request(&message));
    assert!(!upstream_message_is_terminal(&message));
}

#[test]
fn response_created_commits_buffered_websocket_request() {
    let message = TungsteniteMessage::Text(
        r#"{"type":"response.created","response":{"id":"resp-1"}}"#.into(),
    );
    assert!(upstream_message_commits_request(&message));
    assert!(!upstream_message_is_terminal(&message));
}

#[tokio::test]
async fn retry_pending_websocket_request_without_replacement_does_not_settle_failed_account() {
    let (state, account_id) = seeded_state().await;
    let mut pending_request = Some(PendingWebsocketRequest {
        request_messages: vec![TungsteniteMessage::Text(
            r#"{"type":"response.create","model":"gpt-5.4"}"#.into(),
        )],
        buffered_upstream_messages: vec![TungsteniteMessage::Text(
            r#"{"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":95.0}}}"#
                .into(),
        )],
        ..Default::default()
    });

    let outcome = retry_pending_websocket_request(
        &state,
        &HeaderMap::new(),
        &account_id,
        &mut pending_request,
    )
    .await;

    assert!(matches!(
        outcome,
        PendingWebsocketRetryResult::NoReplacement { .. }
    ));
    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.in_flight_requests, 1);
    assert!(view.blocked_reason.is_none());
    assert!(view.blocked_until.is_none());
    assert!(view.last_error.is_none());
}

#[tokio::test]
async fn retry_pending_websocket_request_without_replacement_preserves_buffered_messages() {
    let (state, account_id) = seeded_state().await;
    let buffered = TungsteniteMessage::Text(
        r#"{"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":95.0}}}"#.into(),
    );
    let mut pending_request = Some(PendingWebsocketRequest {
        request_messages: vec![TungsteniteMessage::Text(
            r#"{"type":"response.create","model":"gpt-5.4"}"#.into(),
        )],
        buffered_upstream_messages: vec![buffered.clone()],
        ..Default::default()
    });

    let outcome = retry_pending_websocket_request(
        &state,
        &HeaderMap::new(),
        &account_id,
        &mut pending_request,
    )
    .await;

    assert!(matches!(
        outcome,
        PendingWebsocketRetryResult::NoReplacement { .. }
    ));
    let pending = pending_request.as_ref().expect("pending request remains");
    assert_eq!(pending.buffered_upstream_messages, vec![buffered]);
}

#[tokio::test]
async fn retry_pending_websocket_request_without_replacement_returns_pool_summary_for_remaining_accounts()
 {
    let (state, account_id) = seeded_state().await;
    {
        let mut accounts = state.accounts.write().await;
        let refreshing = accounts
            .import_account("rt_refreshing".to_string(), None, None)
            .expect("import succeeds")
            .account
            .id;
        accounts
            .test_mark_refresh_in_flight(&refreshing)
            .expect("mark refresh in flight");
    }

    let mut pending_request = Some(PendingWebsocketRequest {
        request_messages: vec![TungsteniteMessage::Text(
            r#"{"type":"response.create","model":"gpt-5.4"}"#.into(),
        )],
        ..Default::default()
    });

    let outcome = retry_pending_websocket_request(
        &state,
        &HeaderMap::new(),
        &account_id,
        &mut pending_request,
    )
    .await;

    let PendingWebsocketRetryResult::NoReplacement {
        client_message: Some(client_message),
    } = outcome
    else {
        panic!("expected NoReplacement with client message");
    };

    let Some(WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    }) = classify_websocket_upstream_message(&client_message)
    else {
        panic!("expected failed websocket outcome");
    };
    assert_eq!(failure, FailureClass::TemporaryFailure);
    assert_eq!(retry_after, None);
    assert!(details.contains("status=503"));
    assert!(details.contains("error.code=server_is_overloaded"));
}

#[test]
fn normalize_rate_limit_event_payload_rewrites_used_percent_to_zero() {
    let normalized = normalize_rate_limit_event_payload(
        r#"{"type":"codex.rate_limits","rate_limits":{"primary":{"used_percent":95.0,"window_minutes":15,"reset_at":123},"secondary":{"used_percent":82.5,"window_minutes":10080,"reset_at":456}},"plan_type":"plus"}"#,
    )
    .expect("normalized payload");
    let json: Value = serde_json::from_str(&normalized).expect("json");

    assert_eq!(
        json.get("type").and_then(Value::as_str),
        Some("codex.rate_limits")
    );
    assert_eq!(
        json.pointer("/rate_limits/primary/used_percent")
            .and_then(Value::as_f64),
        Some(0.0)
    );
    assert_eq!(
        json.pointer("/rate_limits/secondary/used_percent")
            .and_then(Value::as_f64),
        Some(0.0)
    );
    assert_eq!(
        json.pointer("/rate_limits/primary/window_minutes")
            .and_then(Value::as_i64),
        Some(15)
    );
}
