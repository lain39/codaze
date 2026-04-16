use super::*;
use crate::accounts::{BlockedReason, BlockedSource, INVALID_REFRESH_TOKEN_MESSAGE, RoutingState};
use crate::classifier::FailureClass;
use axum::{Json, extract::State};
use chrono::{Duration as ChronoDuration, Utc};
use serde_json::Value;
use std::time::Duration as StdDuration;
use tokio::time::timeout;

#[tokio::test]
async fn admin_import_rejects_blank_refresh_token() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");

    let response = super::super::admin::post_admin_accounts_import(
        State(state.clone()),
        Json(super::super::ImportAccountRequest {
            refresh_token: "   ".to_string(),
            label: Some("label".to_string()),
            email: Some("user@example.com".to_string()),
        }),
    )
    .await;

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json.pointer("/error/message").and_then(Value::as_str),
        Some(INVALID_REFRESH_TOKEN_MESSAGE)
    );
    assert!(state.accounts.write().await.list().is_empty());
}

#[tokio::test]
async fn admin_import_hides_internal_disk_error_details() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let first = state
        .import_account(
            "rt_123".to_string(),
            Some("first".to_string()),
            Some("first@example.com".to_string()),
        )
        .await
        .expect("first import succeeds");
    let account_id = first.account.id;

    let blocking_parent = temp.path().join("not-a-dir");
    std::fs::write(&blocking_parent, "file").expect("write blocking parent");
    {
        let mut accounts = state.accounts.write().await;
        let record = accounts
            .test_record_mut(&account_id)
            .expect("record exists");
        record.file_path = blocking_parent.join("account.json");
    }

    let response = super::super::admin::post_admin_accounts_import(
        State(state.clone()),
        Json(super::super::ImportAccountRequest {
            refresh_token: "rt_123".to_string(),
            label: Some("updated".to_string()),
            email: Some("updated@example.com".to_string()),
        }),
    )
    .await;

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json.pointer("/error/message").and_then(Value::as_str),
        Some("internal server error")
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(!text.contains("not-a-dir"));
    assert!(!text.contains("account.json"));
}

#[tokio::test]
async fn admin_account_wake_clears_block_state() {
    let (state, account_id) = seeded_state().await;
    {
        let mut accounts = state.accounts.write().await;
        let record = accounts
            .test_record_mut(&account_id)
            .expect("record exists");
        record.routing_state = RoutingState::Cooldown;
        record.blocked_reason = Some(BlockedReason::RateLimited);
        record.blocked_source = Some(BlockedSource::LocalBackoff);
        record.blocked_until = Some(Utc::now() + ChronoDuration::minutes(3));
        record.local_backoff_level = 2;
        record.last_error = Some("rate limited".to_string());
    }

    let response = super::super::admin::post_admin_account_wake(
        State(state.clone()),
        axum::extract::Path(account_id.clone()),
    )
    .await;

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(
        json.pointer("/disposition").and_then(Value::as_str),
        Some("woken")
    );
    assert!(
        json.pointer("/account/blocked_reason")
            .is_some_and(Value::is_null)
    );
    assert_eq!(
        json.pointer("/account/last_error").and_then(Value::as_str),
        Some("rate limited")
    );
}

#[tokio::test]
async fn admin_accounts_wake_reports_skipped_auth_invalid() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    {
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
        let first_record = accounts.test_record_mut(&first).expect("first record");
        first_record.access_token = Some("at_123".to_string());
        first_record.routing_state = RoutingState::Cooldown;
        first_record.blocked_reason = Some(BlockedReason::QuotaExhausted);
        first_record.blocked_source = Some(BlockedSource::UpstreamRetryAfter);
        first_record.blocked_until = Some(Utc::now() + ChronoDuration::minutes(5));

        let second_record = accounts.test_record_mut(&second).expect("second record");
        second_record.routing_state = RoutingState::AuthInvalid;
        second_record.blocked_reason = Some(BlockedReason::AuthInvalid);
        second_record.blocked_source = Some(BlockedSource::FixedPolicy);
    }

    let response = super::super::admin::post_admin_accounts_wake(State(state.clone())).await;

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    let json: Value = serde_json::from_slice(&body).expect("json");
    assert_eq!(json.pointer("/woken").and_then(Value::as_u64), Some(1));
    assert_eq!(
        json.pointer("/skipped_auth_invalid")
            .and_then(Value::as_u64),
        Some(1)
    );
}

#[tokio::test]
async fn appstate_duplicate_import_write_failure_does_not_update_memory_metadata() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let first = state
        .import_account(
            "rt_123".to_string(),
            Some("first".to_string()),
            Some("first@example.com".to_string()),
        )
        .await
        .expect("first import succeeds");
    let account_id = first.account.id;

    let blocking_parent = temp.path().join("not-a-dir");
    std::fs::write(&blocking_parent, "file").expect("write blocking parent");
    {
        let mut accounts = state.accounts.write().await;
        let record = accounts
            .test_record_mut(&account_id)
            .expect("record exists");
        record.file_path = blocking_parent.join("account.json");
    }

    let error = state
        .import_account(
            "rt_123".to_string(),
            Some("updated".to_string()),
            Some("updated@example.com".to_string()),
        )
        .await
        .expect_err("duplicate metadata update should fail");
    assert!(error.to_string().contains("create account parent dir"));

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view");
    assert_eq!(view.label.as_deref(), Some("first"));
    assert_eq!(view.email.as_deref(), Some("first@example.com"));
}

#[tokio::test]
async fn appstate_auth_invalid_trash_failure_keeps_record_attached() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let first = state
        .import_account(
            "rt_123".to_string(),
            Some("label".to_string()),
            Some("user@example.com".to_string()),
        )
        .await
        .expect("import succeeds");
    let account_id = first.account.id;

    let trash_blocker = temp.path().join("trash");
    std::fs::write(&trash_blocker, "file").expect("write trash blocker");

    state
        .finish_refresh_failure(
            &account_id,
            FailureClass::AuthInvalid,
            None,
            "invalid".to_string(),
        )
        .await
        .expect("trash move failure should be absorbed");

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view");
    assert_eq!(view.routing_state, RoutingState::AuthInvalid);
    assert_eq!(view.blocked_reason, Some(BlockedReason::AuthInvalid));
    assert!(view.blocked_until.is_none());
    assert!(
        view.last_error
            .as_deref()
            .is_some_and(|value| value.contains("invalid"))
    );
    assert!(
        view.last_error
            .as_deref()
            .is_some_and(|value| value.contains("move invalid account file to trash failed"))
    );
    assert!(temp.path().join(format!("{account_id}.json")).exists());
}

#[tokio::test]
async fn appstate_non_auth_refresh_failure_does_not_wait_for_disk_lock() {
    let (state, account_id) = seeded_state().await;
    let disk_guard = state.account_disk_lock.lock().await;

    timeout(
        StdDuration::from_millis(100),
        state.finish_refresh_failure(
            &account_id,
            FailureClass::RateLimited,
            None,
            "rate limited".to_string(),
        ),
    )
    .await
    .expect("non-auth failure should not block on disk lock")
    .expect("failure applied");

    drop(disk_guard);

    let view = state
        .accounts
        .write()
        .await
        .view(&account_id)
        .expect("view");
    assert_eq!(view.routing_state, RoutingState::Cooldown);
    assert_eq!(view.blocked_reason, Some(BlockedReason::RateLimited));
    assert_eq!(view.in_flight_requests, 0);
}
