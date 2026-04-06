use super::*;
use crate::accounts::{AccountStore, PoolBlockSummary, SelectionFailure};
use crate::classifier::FailureClass;
use crate::failover::{
    FailoverFailure, SuccessDisposition, apply_refresh_failure, execute_unary_json_with_failover,
    execute_with_failover, execute_with_failover_after_selection, resolve_selection_failure,
};
use crate::upstream::RefreshFailure;
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::{
    Json,
    extract::{OriginalUri, State},
};
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::time::Duration;

#[tokio::test]
async fn app_refresh_quota_failure_applies_upstream_retry_after_to_account_state() {
    let (state, account_id) = cold_state().await;
    {
        let mut accounts = state.accounts.write().await;
        let selection = accounts
            .select_account(
                RoutingPolicy::LeastInFlight,
                state.config.refresh_skew_seconds,
                &std::collections::HashSet::new(),
            )
            .expect("selection succeeds");
        assert_eq!(selection.account_id, account_id);
        assert!(selection.needs_refresh);
    }

    let before = Utc::now();
    apply_refresh_failure(
        &state,
        &account_id,
        &RefreshFailure {
            status: StatusCode::BAD_GATEWAY,
            body: r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#
                .to_string(),
            class: FailureClass::QuotaExhausted,
            retry_after: Some(Duration::from_secs(77)),
        },
    )
    .await
    .expect("refresh failure applied");

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("account");
    assert_eq!(view.routing_state, crate::accounts::RoutingState::Cooldown);
    assert_eq!(
        view.blocked_reason,
        Some(crate::accounts::BlockedReason::QuotaExhausted)
    );
    assert_eq!(
        view.blocked_source,
        Some(crate::accounts::BlockedSource::UpstreamRetryAfter)
    );
    assert_eq!(view.in_flight_requests, 0);
    assert!(!view.refresh_in_flight);
    assert_eq!(
        view.last_error.as_deref(),
        Some(
            "refresh failed: status=502 Bad Gateway body={\"error\":{\"type\":\"usage_limit_reached\",\"message\":\"The usage limit has been reached\",\"resets_in_seconds\":77}}"
        )
    );
    let blocked_until = view.blocked_until.expect("blocked until");
    assert!(blocked_until >= before + ChronoDuration::seconds(76));
    assert!(blocked_until <= before + ChronoDuration::seconds(78));
}

#[tokio::test]
async fn apply_refresh_failure_request_rejected_records_last_error_without_blocking() {
    let (state, account_id) = cold_state().await;
    {
        let mut accounts = state.accounts.write().await;
        let selection = accounts
            .select_account(
                RoutingPolicy::LeastInFlight,
                state.config.refresh_skew_seconds,
                &std::collections::HashSet::new(),
            )
            .expect("selection succeeds");
        assert_eq!(selection.account_id, account_id);
        assert!(selection.needs_refresh);
    }

    apply_refresh_failure(
        &state,
        &account_id,
        &RefreshFailure {
            status: StatusCode::BAD_REQUEST,
            body: r#"{"error":{"code":"invalid_request_error","message":"bad refresh request"}}"#
                .to_string(),
            class: FailureClass::RequestRejected,
            retry_after: None,
        },
    )
    .await
    .expect("refresh failure applied");

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("account");
    assert_eq!(view.routing_state, crate::accounts::RoutingState::Cold);
    assert!(view.blocked_reason.is_none());
    assert!(view.blocked_until.is_none());
    assert_eq!(view.in_flight_requests, 0);
    assert!(!view.refresh_in_flight);
    assert_eq!(
        view.last_error.as_deref(),
        Some(
            "refresh failed: status=400 Bad Request body={\"error\":{\"code\":\"invalid_request_error\",\"message\":\"bad refresh request\"}}"
        )
    );
}

#[tokio::test]
async fn apply_refresh_failure_auth_invalid_trash_failure_keeps_other_account_routeable() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let (first_account_id, second_account_id, second_access_token) = {
        let mut accounts = state.accounts.write().await;
        let first = accounts
            .import_account("rt_123".to_string(), None, None)
            .expect("first import succeeds")
            .account
            .id;
        let second = accounts
            .import_account("rt_456".to_string(), None, None)
            .expect("second import succeeds")
            .account
            .id;
        accounts
            .test_mark_refresh_in_flight(&first)
            .expect("mark first refresh in flight");
        accounts
            .finish_refresh_success(
                &second,
                RefreshedAccount {
                    access_token: "at_456".to_string(),
                    refresh_token: None,
                    account_id: Some("acct_456".to_string()),
                    plan_type: Some("plus".to_string()),
                    email: Some("user2@example.com".to_string()),
                    access_token_expires_at: Some(Utc::now() + ChronoDuration::minutes(30)),
                },
            )
            .expect("second refresh success seeded");
        let second_access_token = accounts
            .upstream_account(&second)
            .expect("second upstream account")
            .access_token;
        (first, second, second_access_token)
    };

    let trash_blocker = temp.path().join("trash");
    std::fs::write(&trash_blocker, "file").expect("write trash blocker");

    apply_refresh_failure(
        &state,
        &first_account_id,
        &RefreshFailure {
            status: StatusCode::UNAUTHORIZED,
            body: r#"{"error":{"message":"Could not validate your refresh token. Please try signing in again.","type":"invalid_request_error","code":"refresh_token_expired"}}"#
                .to_string(),
            class: FailureClass::AuthInvalid,
            retry_after: None,
        },
    )
    .await
    .expect("trash failure should not abort failover flow");

    let first = state
        .accounts
        .write()
        .await
        .view(&first_account_id)
        .expect("first account remains attached");
    assert_eq!(
        first.routing_state,
        crate::accounts::RoutingState::AuthInvalid
    );
    assert_eq!(
        first.blocked_reason,
        Some(crate::accounts::BlockedReason::AuthInvalid)
    );
    assert!(first.blocked_until.is_none());
    assert!(
        first
            .last_error
            .as_deref()
            .is_some_and(|value| value.contains("move invalid account file to trash failed"))
    );

    let routed = execute_with_failover(
        &state,
        SuccessDisposition::ReleaseImmediately,
        move |account| {
            let second_access_token = second_access_token.clone();
            async move {
                assert_eq!(account.access_token, second_access_token);
                Ok::<_, codex_client::TransportError>(())
            }
        },
    )
    .await
    .expect("other account should remain routeable");

    assert_eq!(routed.account_id, second_account_id);
}

#[tokio::test]
async fn responses_when_pool_is_blocked_returns_synthetic_quota_event() {
    use super::super::api::post_responses;

    let (state, account_id) = seeded_state().await;
    {
        let mut accounts = state.accounts.write().await;
        accounts
            .mark_request_failure(
                &account_id,
                FailureClass::QuotaExhausted,
                Some(Duration::from_secs(600)),
                "seed quota block".to_string(),
            )
            .expect("mark failure");
    }

    let response = post_responses(
        State(state),
        OriginalUri(Uri::from_static("/v1/responses")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("codex-tui"));
            headers
        },
        Json(json!({
            "model": "gpt-5.4",
            "input": [{
                "role": "user",
                "content": [{"type": "input_text", "text": "hi"}]
            }]
        })),
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );

    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("event: response.failed"));
    assert!(text.contains("No account available right now. Try again later."));
    assert!(!text.contains("no eligible account available"));
    assert!(!text.contains("usage_limit_reached"));
}

#[tokio::test]
async fn pool_blocked_quota_into_response_is_structured_json() {
    let until = Utc::now() + ChronoDuration::minutes(10);
    let response = FailoverFailure::PoolBlocked(PoolBlockSummary {
        blocked_reason: crate::accounts::BlockedReason::QuotaExhausted,
        blocked_until: Some(until),
        retry_after: Some(Duration::from_secs(600)),
    })
    .into_response();

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let (_, headers, _) = response_parts(
        FailoverFailure::PoolBlocked(PoolBlockSummary {
            blocked_reason: crate::accounts::BlockedReason::QuotaExhausted,
            blocked_until: Some(until),
            retry_after: Some(Duration::from_secs(600)),
        })
        .into_response(),
    )
    .await;
    assert!(headers.get("retry-after").is_none());
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json.pointer("/error/code").and_then(Value::as_str),
        Some("server_is_overloaded")
    );
    assert_eq!(
        json.pointer("/error/message").and_then(Value::as_str),
        Some("No account available right now. Try again later.")
    );
    assert!(json.pointer("/error/type").is_none());
    assert!(json.pointer("/error/resets_in_seconds").is_none());
    assert!(json.pointer("/error/resets_at").is_none());
}

#[tokio::test]
async fn pool_blocked_auth_invalid_into_response_is_structured_json() {
    let response = FailoverFailure::PoolBlocked(PoolBlockSummary {
        blocked_reason: crate::accounts::BlockedReason::AuthInvalid,
        blocked_until: None,
        retry_after: None,
    })
    .into_response();

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(headers.get("retry-after").is_none());
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json.pointer("/error/code").and_then(Value::as_str),
        Some("server_is_overloaded")
    );
    assert_eq!(
        json.pointer("/error/message").and_then(Value::as_str),
        Some("No account available right now. Try again later.")
    );
    assert!(json.pointer("/error/resets_in_seconds").is_none());
    assert!(json.pointer("/error/resets_at").is_none());
}

#[test]
fn resolve_selection_failure_uses_auth_invalid_candidate_before_generic_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut accounts = AccountStore::new(temp.path().to_path_buf());

    let failure = resolve_selection_failure(
        &mut accounts,
        SelectionFailure::NoEligibleAccount,
        &HashSet::new(),
        Some(PoolBlockSummary {
            blocked_reason: crate::accounts::BlockedReason::AuthInvalid,
            blocked_until: None,
            retry_after: None,
        }),
        None,
    );

    match failure {
        FailoverFailure::PoolBlocked(summary) => {
            assert_eq!(
                summary.blocked_reason,
                crate::accounts::BlockedReason::AuthInvalid
            );
            assert!(summary.retry_after.is_none());
            assert!(summary.blocked_until.is_none());
        }
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[test]
fn resolve_selection_failure_no_eligible_returns_generic_pool_block_before_retryable_response() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut accounts = AccountStore::new(temp.path().to_path_buf());

    let failure = resolve_selection_failure(
        &mut accounts,
        SelectionFailure::NoEligibleAccount,
        &HashSet::new(),
        None,
        Some(FailoverFailure::Refresh(RefreshFailure {
            status: StatusCode::BAD_GATEWAY,
            body: r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#
                .to_string(),
            class: FailureClass::QuotaExhausted,
            retry_after: Some(Duration::from_secs(77)),
        })),
    );

    match failure {
        FailoverFailure::PoolBlocked(summary) => {
            assert_eq!(
                summary.blocked_reason,
                crate::accounts::BlockedReason::TemporarilyUnavailable
            );
            assert!(summary.retry_after.is_none());
            assert!(summary.blocked_until.is_none());
        }
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[test]
fn resolve_selection_failure_uses_refresh_in_flight_summary_before_generic_json() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut accounts = AccountStore::new(temp.path().to_path_buf());
    let id = accounts
        .import_account("rt_123".to_string(), None, None)
        .expect("import succeeds")
        .account
        .id;
    accounts
        .test_mark_refresh_in_flight(&id)
        .expect("mark refresh in flight");

    let failure = resolve_selection_failure(
        &mut accounts,
        SelectionFailure::NoEligibleAccount,
        &HashSet::new(),
        None,
        None,
    );

    match failure {
        FailoverFailure::PoolBlocked(summary) => {
            assert_eq!(
                summary.blocked_reason,
                crate::accounts::BlockedReason::TemporarilyUnavailable
            );
            assert_eq!(summary.retry_after, Some(Duration::from_secs(1)));
            assert!(summary.blocked_until.is_some());
        }
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[test]
fn resolve_selection_failure_prefers_refresh_in_flight_over_blocked_summary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut accounts = AccountStore::new(temp.path().to_path_buf());
    let blocked = accounts
        .import_account("rt_blocked".to_string(), None, None)
        .expect("import succeeds")
        .account
        .id;
    let refreshing = accounts
        .import_account("rt_refreshing".to_string(), None, None)
        .expect("import succeeds")
        .account
        .id;

    {
        let record = accounts.test_record_mut(&blocked).expect("blocked record");
        record.blocked_reason = Some(crate::accounts::BlockedReason::QuotaExhausted);
        record.blocked_source = Some(crate::accounts::BlockedSource::UpstreamRetryAfter);
        record.blocked_until = Some(Utc::now() + ChronoDuration::minutes(10));
        record.routing_state = crate::accounts::RoutingState::Cooldown;
    }
    accounts
        .test_mark_refresh_in_flight(&refreshing)
        .expect("mark refresh in flight");

    let failure = resolve_selection_failure(
        &mut accounts,
        SelectionFailure::NoEligibleAccount,
        &HashSet::new(),
        None,
        None,
    );

    match failure {
        FailoverFailure::PoolBlocked(summary) => {
            assert_eq!(
                summary.blocked_reason,
                crate::accounts::BlockedReason::TemporarilyUnavailable
            );
            assert_eq!(summary.retry_after, Some(Duration::from_secs(1)));
            assert!(summary.blocked_until.is_some());
        }
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[test]
fn resolve_selection_failure_excludes_current_failed_account_from_pool_summary() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut accounts = AccountStore::new(temp.path().to_path_buf());
    let blocked = accounts
        .import_account("rt_blocked".to_string(), None, None)
        .expect("import succeeds")
        .account
        .id;
    let refreshing = accounts
        .import_account("rt_refreshing".to_string(), None, None)
        .expect("import succeeds")
        .account
        .id;

    {
        let record = accounts.test_record_mut(&blocked).expect("blocked record");
        record.blocked_reason = Some(crate::accounts::BlockedReason::QuotaExhausted);
        record.blocked_source = Some(crate::accounts::BlockedSource::UpstreamRetryAfter);
        record.blocked_until = Some(Utc::now() + ChronoDuration::minutes(10));
        record.routing_state = crate::accounts::RoutingState::Cooldown;
    }
    accounts
        .test_mark_refresh_in_flight(&refreshing)
        .expect("mark refresh in flight");

    let excluded_accounts = HashSet::from([blocked]);
    let failure = resolve_selection_failure(
        &mut accounts,
        SelectionFailure::NoEligibleAccount,
        &excluded_accounts,
        None,
        None,
    );

    match failure {
        FailoverFailure::PoolBlocked(summary) => {
            assert_eq!(
                summary.blocked_reason,
                crate::accounts::BlockedReason::TemporarilyUnavailable
            );
            assert_eq!(summary.retry_after, Some(Duration::from_secs(1)));
            assert!(summary.blocked_until.is_some());
        }
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[tokio::test]
async fn failover_failure_into_response_keeps_compact_http_error_shape() {
    let response = FailoverFailure::Transport(codex_client::TransportError::Http {
        status: StatusCode::FORBIDDEN,
        url: None,
        headers: Some({
            let mut headers = HeaderMap::new();
            headers.insert(
                "x-codex-primary-used-percent",
                HeaderValue::from_static("95.0"),
            );
            headers
        }),
        body: Some("{\"error\":{\"message\":\"forbidden\"}}".to_string()),
    })
    .into_response();

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        headers
            .get("x-codex-primary-used-percent")
            .and_then(|value| value.to_str().ok()),
        Some("0.0")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&body)
            .expect("json")
            .pointer("/error/message")
            .and_then(Value::as_str),
        Some("forbidden")
    );
}

#[tokio::test]
async fn transport_http_failure_response_sanitizes_hop_by_hop_headers() {
    let response = FailoverFailure::Transport(codex_client::TransportError::Http {
        status: StatusCode::BAD_GATEWAY,
        url: None,
        headers: Some({
            let mut headers = HeaderMap::new();
            headers.insert("connection", HeaderValue::from_static("keep-alive, x-next"));
            headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
            headers.insert("transfer-encoding", HeaderValue::from_static("chunked"));
            headers.insert("upgrade", HeaderValue::from_static("websocket"));
            headers.insert("x-next", HeaderValue::from_static("value-b"));
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            headers
        }),
        body: Some("{\"error\":{\"message\":\"upstream failed\"}}".to_string()),
    })
    .into_response();

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_GATEWAY);
    assert!(headers.get("connection").is_none());
    assert!(headers.get("keep-alive").is_none());
    assert!(headers.get("transfer-encoding").is_none());
    assert!(headers.get("upgrade").is_none());
    assert!(headers.get("x-next").is_none());
    assert!(headers.get("content-length").is_none());
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&body)
            .expect("json")
            .pointer("/error/message")
            .and_then(Value::as_str),
        Some("upstream failed")
    );
}

#[tokio::test]
async fn execute_unary_json_with_failover_decode_failure_marks_account_failed_and_retries() {
    let (state, [first_account_id, second_account_id]) = seeded_state_pair().await;
    let first_access_token = {
        let accounts = state.accounts.read().await;
        accounts
            .upstream_account(&first_account_id)
            .expect("first upstream account")
            .access_token
    };

    let response = execute_unary_json_with_failover(&state, move |upstream_account| {
        let first_access_token = first_access_token.clone();
        async move {
            let body = if upstream_account.access_token == first_access_token {
                Bytes::from_static(b"not-json")
            } else {
                Bytes::from_static(br#"{"ok":true}"#)
            };
            Ok::<_, codex_client::TransportError>(crate::upstream::UpstreamUnaryResponse {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                body,
            })
        }
    })
    .await
    .expect("second account succeeds");

    let (status, _, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_slice::<Value>(&body)
            .expect("json")
            .pointer("/ok")
            .and_then(Value::as_bool),
        Some(true)
    );

    let accounts = state.accounts.write().await;
    let first = accounts.view(&first_account_id).expect("first view exists");
    assert_eq!(
        first.routing_state,
        crate::accounts::RoutingState::TemporarilyUnavailable
    );
    assert_eq!(first.in_flight_requests, 0);
    assert!(first.last_success_at.is_none());
    assert_eq!(
        first.last_error.as_deref(),
        Some("decode upstream json body")
    );

    let second = accounts
        .view(&second_account_id)
        .expect("second view exists");
    assert!(second.last_success_at.is_some());
    assert!(second.last_error.is_none());
    assert_eq!(second.in_flight_requests, 0);
}

#[tokio::test]
async fn execute_unary_json_with_failover_all_bad_json_returns_bad_gateway() {
    let (state, [_first_account_id, _second_account_id]) = seeded_state_pair().await;

    let failure = execute_unary_json_with_failover(&state, |_| async {
        Ok::<_, codex_client::TransportError>(crate::upstream::UpstreamUnaryResponse {
            status: StatusCode::OK,
            headers: HeaderMap::new(),
            body: Bytes::from_static(b"not-json"),
        })
    })
    .await
    .expect_err("all accounts return malformed json");

    match failure {
        FailoverFailure::Json { status, message } => {
            assert_eq!(status, StatusCode::BAD_GATEWAY);
            assert_eq!(message, "decode upstream json body");
        }
        other => panic!("unexpected failure: {other:?}"),
    }
}

#[tokio::test]
async fn execute_with_failover_request_rejected_records_last_error_without_blocking() {
    let (state, account_id) = seeded_state().await;
    state.accounts.write().await.release_selection(&account_id);

    let failure = execute_with_failover(&state, SuccessDisposition::HoldUntilCaller, |_| async {
        Err::<(), codex_client::TransportError>(codex_client::TransportError::Http {
            status: StatusCode::BAD_REQUEST,
            url: None,
            headers: None,
            body: Some(r#"{"error":{"code":"invalid_prompt","message":"bad prompt"}}"#.to_string()),
        })
    })
    .await
    .expect_err("should fail");

    match failure {
        FailoverFailure::Transport(codex_client::TransportError::Http { status, .. }) => {
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        other => panic!("unexpected failure: {other:?}"),
    }

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view exists");
    assert_eq!(view.routing_state, crate::accounts::RoutingState::Ready);
    assert!(view.blocked_reason.is_none());
    assert!(view.blocked_until.is_none());
    assert_eq!(view.in_flight_requests, 0);
    assert_eq!(
        view.last_error.as_deref(),
        Some(
            "RequestRejected: status=400 Bad Request body={\"error\":{\"code\":\"invalid_prompt\",\"message\":\"bad prompt\"}}"
        )
    );
}

#[tokio::test]
async fn execute_with_failover_missing_access_token_after_selection_continues_failover() {
    let (state, account_ids) = seeded_state_pair().await;
    *state.routing_policy.write().await = RoutingPolicy::FillFirst;
    let (selected_account_id, fallback_account_id) = {
        let mut accounts = state.accounts.write().await;
        let selection = accounts
            .select_account(
                RoutingPolicy::FillFirst,
                state.config.refresh_skew_seconds,
                &HashSet::new(),
            )
            .expect("selection succeeds");
        let selected = selection.account_id;
        accounts.release_selection(&selected);
        let fallback = account_ids
            .iter()
            .find(|account_id| **account_id != selected)
            .expect("fallback account exists")
            .clone();
        (selected, fallback)
    };
    let fallback_access_token = state
        .accounts
        .write()
        .await
        .upstream_account(&fallback_account_id)
        .expect("fallback upstream account")
        .access_token;

    let selected_account_id_for_hook = selected_account_id.clone();
    let routed = execute_with_failover_after_selection(
        &state,
        SuccessDisposition::ReleaseImmediately,
        move |account| {
            let fallback_access_token = fallback_access_token.clone();
            async move {
                if account.access_token == fallback_access_token {
                    Ok::<_, codex_client::TransportError>("fallback")
                } else {
                    Ok::<_, codex_client::TransportError>("unexpected")
                }
            }
        },
        move |state, account_id| {
            let selected_account_id = selected_account_id_for_hook.clone();
            let account_id = account_id.to_string();
            async move {
                if account_id == selected_account_id {
                    let mut accounts = state.accounts.write().await;
                    let selected = accounts
                        .test_record_mut(&selected_account_id)
                        .expect("selected record exists");
                    selected.access_token = None;
                    selected.access_token_expires_at = None;
                }
            }
        },
    )
    .await
    .expect("failover succeeds");

    assert_eq!(routed.account_id, fallback_account_id);
    assert_eq!(routed.value, "fallback");

    let accounts = state.accounts.write().await;
    let selected = accounts
        .view(&selected_account_id)
        .expect("selected view exists");
    assert_eq!(selected.in_flight_requests, 0);
    assert!(selected.last_success_at.is_none());
    let fallback = accounts
        .view(&fallback_account_id)
        .expect("fallback view exists");
    assert!(fallback.last_success_at.is_some());
    assert_eq!(fallback.in_flight_requests, 0);
}
