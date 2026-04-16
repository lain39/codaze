use crate::accounts::{
    AccountStore, BlockedReason, PoolBlockSummary, SelectionFailure, UpstreamAccount,
    UpstreamAccountError,
};
use crate::app::AppState;
use crate::classifier::{FailureClass, classify_request_error};
use crate::gateway_errors::{FailureRenderMode, render_failover_failure};
use crate::models::ResponseShape;
use crate::responses::extract_retry_after;
use crate::upstream::{
    RefreshFailure, UpstreamUnaryResponse, UpstreamWebsocketConnection, body_as_json,
};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::{Json, response::IntoResponse};
use std::collections::HashSet;
use std::future::Future;
use std::time::Duration;
use tracing::warn;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SuccessDisposition {
    ReleaseImmediately,
    HoldUntilCaller,
}

#[derive(Debug)]
pub(crate) struct RoutedExecution<T> {
    pub(crate) account_id: String,
    pub(crate) value: T,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AccountSettlement {
    Success,
    Release,
    Failure {
        failure: FailureClass,
        retry_after: Option<Duration>,
        details: String,
    },
}

#[derive(Debug)]
pub(crate) enum FailoverFailure {
    Refresh(RefreshFailure),
    Transport(codex_client::TransportError),
    PoolBlocked(PoolBlockSummary),
    CallerJson { status: StatusCode, message: String },
    Internal { status: StatusCode, detail: String },
}

impl FailoverFailure {
    pub(crate) fn into_response(self) -> Response {
        render_failover_failure(&self, ResponseShape::Codex, FailureRenderMode::UnaryJson)
    }
}

pub(crate) async fn connect_responses_websocket_with_failover(
    state: &AppState,
    request_headers: &HeaderMap,
    tried_accounts: &mut HashSet<String>,
) -> Result<RoutedExecution<UpstreamWebsocketConnection>, FailoverFailure> {
    let request_headers = request_headers.clone();
    let upstream_client = state.upstream.clone();
    execute_with_failover_from(
        state,
        SuccessDisposition::HoldUntilCaller,
        tried_accounts,
        move |upstream_account| {
            let request_headers = request_headers.clone();
            let upstream_client = upstream_client.clone();
            async move {
                upstream_client
                    .connect_responses_websocket(&upstream_account, &request_headers)
                    .await
            }
        },
    )
    .await
}

pub(crate) async fn execute_with_failover<T, F, Fut>(
    state: &AppState,
    success_disposition: SuccessDisposition,
    execute: F,
) -> Result<RoutedExecution<T>, FailoverFailure>
where
    F: FnMut(UpstreamAccount) -> Fut,
    Fut: Future<Output = Result<T, codex_client::TransportError>>,
{
    let mut tried_accounts = HashSet::new();
    execute_with_failover_from_inner(
        state,
        success_disposition,
        &mut tried_accounts,
        execute,
        |_, _| async {},
    )
    .await
}

pub(crate) async fn execute_with_failover_from<T, F, Fut>(
    state: &AppState,
    success_disposition: SuccessDisposition,
    tried_accounts: &mut HashSet<String>,
    execute: F,
) -> Result<RoutedExecution<T>, FailoverFailure>
where
    F: FnMut(UpstreamAccount) -> Fut,
    Fut: Future<Output = Result<T, codex_client::TransportError>>,
{
    execute_with_failover_from_inner(
        state,
        success_disposition,
        tried_accounts,
        execute,
        |_, _| async {},
    )
    .await
}

async fn execute_with_failover_from_inner<T, F, Fut, H, Hfut>(
    state: &AppState,
    success_disposition: SuccessDisposition,
    tried_accounts: &mut HashSet<String>,
    mut execute: F,
    mut after_selection: H,
) -> Result<RoutedExecution<T>, FailoverFailure>
where
    F: FnMut(UpstreamAccount) -> Fut,
    Fut: Future<Output = Result<T, codex_client::TransportError>>,
    H: FnMut(AppState, &str) -> Hfut,
    Hfut: Future<Output = ()>,
{
    let routing_policy = *state.routing_policy.read().await;
    let mut forced_refresh_attempted = HashSet::new();
    let mut last_pool_summary_candidate = None;
    let mut last_retryable_response = None;

    loop {
        let selection = {
            let mut accounts = state.accounts.write().await;
            match accounts.select_account(
                routing_policy,
                state.config.refresh_skew_seconds,
                tried_accounts,
            ) {
                Ok(selection) => selection,
                Err(error) => {
                    return Err(resolve_selection_failure(
                        &mut accounts,
                        error,
                        tried_accounts,
                        last_pool_summary_candidate.take(),
                        last_retryable_response.take(),
                    ));
                }
            }
        };

        after_selection(state.clone(), &selection.account_id).await;

        if selection.needs_refresh {
            match state
                .upstream
                .refresh_access_token(selection.refresh_token.clone())
                .await
            {
                Ok(refreshed) => {
                    match state
                        .finish_refresh_success(&selection.account_id, refreshed)
                        .await
                    {
                        Ok(result) => {
                            if let Some(warning) = result.persist_warning {
                                warn!(
                                    account_id = %selection.account_id,
                                    %warning,
                                    "refresh succeeded but account file persistence failed"
                                );
                            }
                        }
                        Err(error) => {
                            state
                                .accounts
                                .write()
                                .await
                                .release_selection(&selection.account_id);
                            return Err(FailoverFailure::Internal {
                                status: StatusCode::INTERNAL_SERVER_ERROR,
                                detail: error.to_string(),
                            });
                        }
                    }
                }
                Err(error) => {
                    let failure = error.class;
                    if matches!(
                        failure,
                        FailureClass::RequestRejected | FailureClass::InternalFailure
                    ) {
                        apply_refresh_failure(state, &selection.account_id, &error).await?;
                        return Err(FailoverFailure::Refresh(error));
                    }

                    let retryable = should_failover_failure_class(failure);
                    apply_refresh_failure(state, &selection.account_id, &error).await?;
                    if failure == FailureClass::AuthInvalid {
                        last_pool_summary_candidate = Some(PoolBlockSummary {
                            blocked_reason: BlockedReason::AuthInvalid,
                            blocked_until: None,
                            retry_after: None,
                        });
                    }
                    let response = FailoverFailure::Refresh(error);
                    if retryable {
                        tried_accounts.insert(selection.account_id);
                        last_retryable_response = Some(response);
                        continue;
                    }
                    return Err(response);
                }
            }
        }

        let upstream_account = {
            let accounts = state.accounts.read().await;
            match accounts.upstream_account(&selection.account_id) {
                Ok(account) => account,
                Err(UpstreamAccountError::MissingAccessToken) => {
                    drop(accounts);
                    let mut accounts = state.accounts.write().await;
                    accounts.invalidate_access_token(&selection.account_id);
                    accounts.release_selection(&selection.account_id);
                    tried_accounts.insert(selection.account_id);
                    continue;
                }
                Err(UpstreamAccountError::MissingRecord) => {
                    drop(accounts);
                    state
                        .accounts
                        .write()
                        .await
                        .release_selection(&selection.account_id);
                    tried_accounts.insert(selection.account_id);
                    continue;
                }
            }
        };

        match execute(upstream_account).await {
            Ok(result) => {
                if success_disposition == SuccessDisposition::ReleaseImmediately {
                    state
                        .accounts
                        .write()
                        .await
                        .mark_request_success(&selection.account_id);
                }
                return Ok(RoutedExecution {
                    account_id: selection.account_id,
                    value: result,
                });
            }
            Err(error) => {
                let failure = classify_request_error(&error);
                let details = extract_failure_reason(&error, failure);
                let retry_after = extract_retry_after(&error);
                let response = FailoverFailure::Transport(error);

                match failure {
                    FailureClass::AccessTokenRejected => {
                        let mut accounts = state.accounts.write().await;
                        if forced_refresh_attempted.contains(&selection.account_id) {
                            accounts.invalidate_access_token(&selection.account_id);
                            if let Err(apply_error) = accounts.mark_request_failure(
                                &selection.account_id,
                                failure,
                                retry_after,
                                details,
                            ) {
                                return Err(FailoverFailure::Internal {
                                    status: StatusCode::INTERNAL_SERVER_ERROR,
                                    detail: apply_error.to_string(),
                                });
                            }
                            tried_accounts.insert(selection.account_id);
                            last_retryable_response = Some(response);
                            continue;
                        }

                        forced_refresh_attempted.insert(selection.account_id.clone());
                        accounts.invalidate_access_token(&selection.account_id);
                        accounts.release_selection(&selection.account_id);
                        continue;
                    }
                    FailureClass::RequestRejected | FailureClass::InternalFailure => {
                        let mut accounts = state.accounts.write().await;
                        if let Err(apply_error) = accounts.mark_request_failure(
                            &selection.account_id,
                            failure,
                            retry_after,
                            details,
                        ) {
                            return Err(FailoverFailure::Internal {
                                status: StatusCode::INTERNAL_SERVER_ERROR,
                                detail: apply_error.to_string(),
                            });
                        }
                        return Err(response);
                    }
                    _ => {}
                }

                let retryable = should_failover_failure_class(failure);
                let mut accounts = state.accounts.write().await;
                if let Err(apply_error) = accounts.mark_request_failure(
                    &selection.account_id,
                    failure,
                    retry_after,
                    details,
                ) {
                    return Err(FailoverFailure::Internal {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        detail: apply_error.to_string(),
                    });
                }
                if retryable {
                    tried_accounts.insert(selection.account_id);
                    last_retryable_response = Some(response);
                    continue;
                }
                return Err(response);
            }
        }
    }
}

#[cfg(test)]
pub(crate) async fn execute_with_failover_after_selection<T, F, Fut, H, Hfut>(
    state: &AppState,
    success_disposition: SuccessDisposition,
    execute: F,
    after_selection: H,
) -> Result<RoutedExecution<T>, FailoverFailure>
where
    F: FnMut(UpstreamAccount) -> Fut,
    Fut: Future<Output = Result<T, codex_client::TransportError>>,
    H: FnMut(AppState, &str) -> Hfut,
    Hfut: Future<Output = ()>,
{
    let mut tried_accounts = HashSet::new();
    execute_with_failover_from_inner(
        state,
        success_disposition,
        &mut tried_accounts,
        execute,
        after_selection,
    )
    .await
}

pub(crate) async fn execute_unary_json_with_failover<F, Fut>(
    state: &AppState,
    execute: F,
) -> Result<Response, FailoverFailure>
where
    F: FnMut(UpstreamAccount) -> Fut,
    Fut: Future<Output = Result<UpstreamUnaryResponse, codex_client::TransportError>>,
{
    execute_unary_json_with_failover_shaped(state, execute, |status, headers, json_body| {
        (status, headers, Json(json_body)).into_response()
    })
    .await
}

pub(crate) async fn execute_unary_json_with_failover_shaped<F, Fut, S>(
    state: &AppState,
    mut execute: F,
    mut shape_response: S,
) -> Result<Response, FailoverFailure>
where
    F: FnMut(UpstreamAccount) -> Fut,
    Fut: Future<Output = Result<UpstreamUnaryResponse, codex_client::TransportError>>,
    S: FnMut(StatusCode, HeaderMap, serde_json::Value) -> Response,
{
    let mut tried_accounts = HashSet::new();
    let mut decode_failed_accounts = HashSet::new();
    let mut last_decode_failure = None;

    loop {
        let upstream = match execute_with_failover_from(
            state,
            SuccessDisposition::HoldUntilCaller,
            &mut tried_accounts,
            &mut execute,
        )
        .await
        {
            Ok(upstream) => upstream,
            Err(error) => {
                if matches!(error, FailoverFailure::PoolBlocked(_))
                    && !decode_failed_accounts.is_empty()
                    && decode_failed_accounts == tried_accounts
                    && let Some(message) = last_decode_failure
                {
                    return Err(FailoverFailure::Internal {
                        status: StatusCode::BAD_GATEWAY,
                        detail: message,
                    });
                }
                return Err(error);
            }
        };

        match body_as_json(&upstream.value.body) {
            Ok(json_body) => {
                let mut accounts = state.accounts.write().await;
                if let Err(error) = apply_account_settlement(
                    &mut accounts,
                    &upstream.account_id,
                    AccountSettlement::Success,
                ) {
                    return Err(FailoverFailure::Internal {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        detail: error.to_string(),
                    });
                }
                return Ok(shape_response(
                    upstream.value.status,
                    upstream.value.headers,
                    json_body,
                ));
            }
            Err(error) => {
                let details = error.to_string();
                let account_id = upstream.account_id;
                let mut accounts = state.accounts.write().await;
                if let Err(error) = apply_account_settlement(
                    &mut accounts,
                    &account_id,
                    AccountSettlement::Failure {
                        failure: FailureClass::TemporaryFailure,
                        retry_after: None,
                        details: details.clone(),
                    },
                ) {
                    return Err(FailoverFailure::Internal {
                        status: StatusCode::INTERNAL_SERVER_ERROR,
                        detail: error.to_string(),
                    });
                }
                decode_failed_accounts.insert(account_id.clone());
                tried_accounts.insert(account_id);
                last_decode_failure = Some(details);
            }
        }
    }
}

pub(crate) async fn apply_refresh_failure(
    state: &AppState,
    account_id: &str,
    error: &RefreshFailure,
) -> Result<(), FailoverFailure> {
    if let Err(apply_error) = state
        .finish_refresh_failure(
            account_id,
            error.class,
            error.retry_after,
            format!(
                "refresh failed: status={} body={}",
                error.status, error.body
            ),
        )
        .await
    {
        return Err(FailoverFailure::Internal {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            detail: apply_error.to_string(),
        });
    }
    Ok(())
}

pub(crate) fn should_failover_failure_class(failure: FailureClass) -> bool {
    matches!(
        failure,
        FailureClass::AccessTokenRejected
            | FailureClass::AuthInvalid
            | FailureClass::RateLimited
            | FailureClass::QuotaExhausted
            | FailureClass::RiskControlled
            | FailureClass::TemporaryFailure
    )
}

pub(crate) fn extract_failure_reason(
    error: &codex_client::TransportError,
    failure: FailureClass,
) -> String {
    match error {
        codex_client::TransportError::Http { status, body, .. } => format!(
            "{failure:?}: status={} body={}",
            status,
            body.clone().unwrap_or_default()
        ),
        _ => format!("{failure:?}: {error}"),
    }
}

pub(crate) fn apply_account_settlement(
    accounts: &mut AccountStore,
    account_id: &str,
    settlement: AccountSettlement,
) -> anyhow::Result<()> {
    match settlement {
        AccountSettlement::Success => {
            accounts.mark_request_success(account_id);
            Ok(())
        }
        AccountSettlement::Release => {
            accounts.release_selection(account_id);
            Ok(())
        }
        AccountSettlement::Failure {
            failure,
            retry_after,
            details,
        } => accounts.mark_request_failure(account_id, failure, retry_after, details),
    }
}

pub(crate) fn spawn_account_settlement(
    state: AppState,
    account_id: String,
    settlement: AccountSettlement,
) {
    tokio::spawn(async move {
        let mut accounts = state.accounts.write().await;
        let result = apply_account_settlement(&mut accounts, &account_id, settlement);
        if let Err(error) = result {
            warn!(account_id = %account_id, %error, "failed to settle account state");
        }
    });
}

pub(crate) fn resolve_selection_failure(
    accounts: &mut AccountStore,
    error: SelectionFailure,
    excluded_accounts: &HashSet<String>,
    last_pool_summary_candidate: Option<PoolBlockSummary>,
    _last_retryable_response: Option<FailoverFailure>,
) -> FailoverFailure {
    match error {
        SelectionFailure::Internal(error) => FailoverFailure::Internal {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            detail: error.to_string(),
        },
        SelectionFailure::NoEligibleAccount => {
            if let Some(summary) = accounts.summarize_selection_failure(excluded_accounts) {
                return FailoverFailure::PoolBlocked(summary);
            }
            FailoverFailure::PoolBlocked(last_pool_summary_candidate.unwrap_or(PoolBlockSummary {
                blocked_reason: BlockedReason::TemporarilyUnavailable,
                blocked_until: None,
                retry_after: None,
            }))
        }
    }
}
