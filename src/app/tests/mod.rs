mod admin;
mod failover;
mod normalization;
mod responses;
mod websocket;

use super::*;
use crate::accounts::RefreshedAccount;
use crate::config::{FingerprintMode, RoutingPolicy, RuntimeConfig};
use axum::body::to_bytes;
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use chrono::{Duration as ChronoDuration, Utc};
use codex_api::ApiError;
use codex_api::ResponseEvent;
use codex_api::sse::responses::process_sse;
use futures::StreamExt;
use futures::stream;
use std::time::Duration;
use tempfile::tempdir;
use tokio::sync::mpsc;

fn test_config(accounts_dir: std::path::PathBuf) -> RuntimeConfig {
    RuntimeConfig {
        listen: "127.0.0.1:18039".to_string(),
        admin_listen: "127.0.0.1:18040".to_string(),
        accounts_dir,
        routing_policy: RoutingPolicy::LeastInFlight,
        fingerprint_mode: FingerprintMode::Normalize,
        upstream_base_url: "https://chatgpt.com/backend-api/codex".to_string(),
        codex_version: "0.118.0".to_string(),
        request_timeout_seconds: 600,
        refresh_skew_seconds: 8,
        accounts_scan_interval_seconds: 15,
        shutdown_grace_period_seconds: 10,
    }
}

async fn seeded_state() -> (AppState, String) {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let account_id = {
        let mut accounts = state.accounts.write().await;
        let view = accounts
            .import_account("rt_123".to_string(), None, None)
            .expect("import succeeds")
            .account;
        accounts
            .finish_refresh_success(
                &view.id,
                RefreshedAccount {
                    access_token: "at_123".to_string(),
                    refresh_token: None,
                    account_id: Some("acct_123".to_string()),
                    plan_type: Some("plus".to_string()),
                    email: Some("user@example.com".to_string()),
                    access_token_expires_at: Some(Utc::now() + ChronoDuration::minutes(30)),
                },
            )
            .expect("refresh success seeded");
        let selection = accounts
            .select_account(
                RoutingPolicy::LeastInFlight,
                8,
                &std::collections::HashSet::new(),
            )
            .expect("selection succeeds");
        assert!(!selection.needs_refresh);
        selection.account_id
    };
    std::mem::forget(temp);
    (state, account_id)
}

async fn seeded_state_pair() -> (AppState, [String; 2]) {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let account_ids = {
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
            .finish_refresh_success(
                &first,
                RefreshedAccount {
                    access_token: "at_123".to_string(),
                    refresh_token: None,
                    account_id: Some("acct_123".to_string()),
                    plan_type: Some("plus".to_string()),
                    email: Some("user1@example.com".to_string()),
                    access_token_expires_at: Some(Utc::now() + ChronoDuration::minutes(30)),
                },
            )
            .expect("first refresh success seeded");
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
        [first, second]
    };
    std::mem::forget(temp);
    (state, account_ids)
}

async fn cold_state() -> (AppState, String) {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");
    let account_id = {
        let mut accounts = state.accounts.write().await;
        accounts
            .import_account("rt_123".to_string(), None, None)
            .expect("import succeeds")
            .account
            .id
    };
    std::mem::forget(temp);
    (state, account_id)
}

async fn yield_for_settlement() {
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
}

async fn collect_codex_sse_results(
    chunks: Vec<Result<Bytes, codex_client::TransportError>>,
) -> Vec<Result<ResponseEvent, ApiError>> {
    let stream = stream::iter(chunks).boxed();
    let (tx, mut rx) = mpsc::channel(16);
    tokio::spawn(process_sse(stream, tx, Duration::from_secs(5), None));

    let mut results = Vec::new();
    while let Some(item) = rx.recv().await {
        let done = item.is_err();
        results.push(item);
        if done {
            break;
        }
    }
    results
}

async fn response_parts(response: Response) -> (StatusCode, HeaderMap, Bytes) {
    let (parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
    (parts.status, parts.headers, bytes)
}
