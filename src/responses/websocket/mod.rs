mod errors;
mod protocol;

#[cfg(test)]
pub(crate) use errors::{
    WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE, WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE,
    classify_websocket_error_text, classify_websocket_upstream_message,
};
pub(crate) use errors::{classify_openai_error_event, classify_response_failed_event};
#[cfg(test)]
pub(crate) use protocol::{
    is_responses_websocket_request_start, normalize_rate_limit_event_payload,
    normalize_response_create_installation_id_payload, normalize_response_create_payload,
    normalize_websocket_rate_limit_message, rewrite_previous_response_not_found_message,
    rewrite_previous_response_not_found_payload, should_passthrough_retryable_websocket_reset,
    upstream_message_commits_request, upstream_message_is_terminal,
};

use self::errors::{
    classify_websocket_upstream_control_message as classify_websocket_upstream_control_message_impl,
    classify_websocket_upstream_terminal_failure as classify_websocket_upstream_terminal_failure_impl,
    websocket_error_message_for_failover_failure,
};
use self::protocol::{
    is_responses_websocket_request_start as is_responses_websocket_request_start_impl,
    map_client_message_to_upstream, map_upstream_message_to_client,
    normalize_response_create_message,
    normalize_websocket_rate_limit_message as normalize_websocket_rate_limit_message_impl,
    rewrite_previous_response_not_found_message as rewrite_previous_response_not_found_message_impl,
    should_buffer_upstream_message_before_commit,
    should_passthrough_retryable_websocket_reset as should_passthrough_retryable_websocket_reset_impl,
    should_replay_client_message,
    upstream_message_commits_request as upstream_message_commits_request_impl,
    upstream_message_is_terminal as upstream_message_is_terminal_impl,
};
use crate::app::AppState;
use crate::classifier::FailureClass;
use crate::failover::{
    AccountSettlement, RoutedExecution, apply_account_settlement,
    connect_responses_websocket_with_failover, should_failover_failure_class,
};
use crate::models::response_shape_for_headers;
use crate::responses::is_gateway_unavailable_status_failure;
use crate::upstream::UpstreamWebsocketConnection;
use axum::extract::ws::{Message as AxumWsMessage, WebSocket};
use futures::{SinkExt, StreamExt};
use http::HeaderMap;
use std::collections::HashSet;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tracing::warn;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum WebsocketProxyOutcome {
    Success,
    Released,
    Failed {
        failure: FailureClass,
        retry_after: Option<Duration>,
        details: String,
    },
}

#[derive(Debug, Default)]
pub(crate) struct PendingWebsocketRequest {
    pub(crate) request_messages: Vec<TungsteniteMessage>,
    pub(crate) buffered_upstream_messages: Vec<TungsteniteMessage>,
    pub(crate) tried_accounts: HashSet<String>,
    pub(crate) committed: bool,
}

pub(crate) enum PendingWebsocketRetryResult {
    Switched {
        failed_account_id: String,
        replacement: Box<RoutedExecution<UpstreamWebsocketConnection>>,
    },
    NoReplacement {
        client_message: Option<TungsteniteMessage>,
    },
}

struct RequestHeadersState<'a> {
    headers: &'a mut HeaderMap,
    allow_upstream_turn_state: bool,
}

fn outcome_after_client_delivery_failure(
    failure: Option<WebsocketProxyOutcome>,
) -> WebsocketProxyOutcome {
    failure.unwrap_or(WebsocketProxyOutcome::Released)
}

fn terminal_outcome_after_successful_delivery(
    outcome: Option<WebsocketProxyOutcome>,
) -> Option<WebsocketProxyOutcome> {
    outcome
}

fn routed_websocket_outcome(
    account_id: &str,
    outcome: WebsocketProxyOutcome,
) -> RoutedExecution<WebsocketProxyOutcome> {
    RoutedExecution {
        account_id: account_id.to_string(),
        value: outcome,
    }
}

fn known_settlement_from_upstream_message(
    message: &TungsteniteMessage,
) -> Option<WebsocketProxyOutcome> {
    let _ = message;
    classify_websocket_upstream_control_message_impl(message)
}

fn active_request_close_failure(
    pending_request: Option<&PendingWebsocketRequest>,
    message: &TungsteniteMessage,
    known_settlement: Option<WebsocketProxyOutcome>,
) -> Option<WebsocketProxyOutcome> {
    if pending_request.is_none() {
        return known_settlement;
    }

    match (message, known_settlement) {
        (TungsteniteMessage::Close(None), Some(WebsocketProxyOutcome::Released) | None) => {
            Some(WebsocketProxyOutcome::Failed {
                failure: FailureClass::TemporaryFailure,
                retry_after: None,
                details: "responses websocket upstream stream failed: closed".to_string(),
            })
        }
        (TungsteniteMessage::Close(Some(frame)), Some(WebsocketProxyOutcome::Released)) => {
            Some(WebsocketProxyOutcome::Failed {
                failure: FailureClass::TemporaryFailure,
                retry_after: None,
                details: format!(
                    "responses websocket upstream closed before terminal response: code={} reason={}",
                    frame.code, frame.reason
                ),
            })
        }
        (_, outcome) => outcome,
    }
}

fn should_failover_precommit_failure(
    outcome: &WebsocketProxyOutcome,
    classified_failure: Option<(http::StatusCode, FailureClass)>,
) -> bool {
    if let Some((status, failure)) = classified_failure {
        return is_gateway_unavailable_status_failure(status, failure);
    }

    match outcome {
        WebsocketProxyOutcome::Failed { failure, .. } => should_failover_failure_class(*failure),
        WebsocketProxyOutcome::Success | WebsocketProxyOutcome::Released => false,
    }
}

async fn flush_buffered_or_settle(
    client_socket: &mut WebSocket,
    pending_request: &mut Option<PendingWebsocketRequest>,
    account_id: &str,
    known_settlement: Option<WebsocketProxyOutcome>,
) -> Option<RoutedExecution<WebsocketProxyOutcome>> {
    if flush_buffered_upstream_messages(
        client_socket,
        take_pending_buffered_upstream_messages(pending_request),
    )
    .await
    {
        None
    } else {
        Some(routed_websocket_outcome(
            account_id,
            outcome_after_client_delivery_failure(known_settlement),
        ))
    }
}

async fn forward_message_or_settle(
    client_socket: &mut WebSocket,
    message: TungsteniteMessage,
    account_id: &str,
    known_settlement: Option<WebsocketProxyOutcome>,
) -> Option<RoutedExecution<WebsocketProxyOutcome>> {
    if forward_upstream_message(client_socket, message).await {
        None
    } else {
        Some(routed_websocket_outcome(
            account_id,
            outcome_after_client_delivery_failure(known_settlement),
        ))
    }
}

async fn deliver_pending_retry_failure_or_settle(
    client_socket: &mut WebSocket,
    pending_request: &mut Option<PendingWebsocketRequest>,
    client_message: Option<TungsteniteMessage>,
    account_id: &str,
    known_settlement: Option<WebsocketProxyOutcome>,
) -> Option<RoutedExecution<WebsocketProxyOutcome>> {
    if let Some(response) = flush_buffered_or_settle(
        client_socket,
        pending_request,
        account_id,
        known_settlement.clone(),
    )
    .await
    {
        return Some(response);
    }
    if let Some(message) = client_message {
        return forward_message_or_settle(client_socket, message, account_id, known_settlement)
            .await;
    }
    None
}

pub(crate) async fn proxy_websocket(
    mut client_socket: WebSocket,
    state: AppState,
    mut request_headers: HeaderMap,
    initial: RoutedExecution<UpstreamWebsocketConnection>,
    allow_upstream_turn_state: bool,
) -> RoutedExecution<WebsocketProxyOutcome> {
    let codex_originator = response_shape_for_headers(&request_headers).is_codex();
    let mut request_headers_state = RequestHeadersState {
        headers: &mut request_headers,
        allow_upstream_turn_state,
    };
    let mut current_account_id = initial.account_id;
    let mut upstream_connection = initial.value;
    let mut pending_request: Option<PendingWebsocketRequest> = None;
    apply_turn_state_to_request_headers(
        request_headers_state.headers,
        upstream_connection.turn_state.as_deref(),
        request_headers_state.allow_upstream_turn_state,
    );

    loop {
        tokio::select! {
            _ = state.shutdown_token.cancelled() => {
                let _ = upstream_connection.stream.close(None).await;
                let _ = client_socket.send(AxumWsMessage::Close(None)).await;
                return routed_websocket_outcome(&current_account_id, WebsocketProxyOutcome::Released);
            }
            client_message = client_socket.next() => {
                let Some(message) = client_message else {
                    let _ = upstream_connection.stream.close(None).await;
                    return routed_websocket_outcome(&current_account_id, WebsocketProxyOutcome::Released);
                };
                let message = match message {
                    Ok(message) => message,
                    Err(_) => {
                        let _ = upstream_connection.stream.close(None).await;
                        return routed_websocket_outcome(&current_account_id, WebsocketProxyOutcome::Released);
                    }
                };

                let sent_close = matches!(message, AxumWsMessage::Close(_));
                let Some(mapped) = map_client_message_to_upstream(message) else {
                    continue;
                };

                if is_responses_websocket_request_start_impl(&mapped) {
                    pending_request = Some(PendingWebsocketRequest::default());
                }

                if let Some(pending) = pending_request.as_mut()
                    && !pending.committed
                    && should_replay_client_message(&mapped)
                {
                    pending.request_messages.push(mapped.clone());
                }

                let mapped = if is_responses_websocket_request_start_impl(&mapped) {
                    let models_snapshot = state.models_cache.read().await.current();
                    normalize_response_create_message(
                        mapped,
                        state.config.fingerprint_mode,
                        codex_originator,
                        models_snapshot.as_deref(),
                        upstream_connection.installation_id.as_deref(),
                    )
                } else {
                    mapped
                };

                if let Err(error) = upstream_connection.stream.send(mapped).await {
                    if sent_close {
                        return routed_websocket_outcome(&current_account_id, WebsocketProxyOutcome::Released);
                    }

                    let failure = WebsocketProxyOutcome::Failed {
                        failure: FailureClass::TemporaryFailure,
                        retry_after: None,
                        details: format!("responses websocket upstream stream failed: {error}"),
                    };

                    if pending_request
                        .as_ref()
                        .is_some_and(|pending| !pending.committed)
                    {
                        match retry_pending_websocket_request(
                            &state,
                            request_headers_state.headers,
                            &current_account_id,
                            &mut pending_request,
                        )
                        .await
                        {
                            PendingWebsocketRetryResult::Switched {
                                failed_account_id,
                                replacement,
                            } => {
                                upstream_connection = apply_websocket_retry_switch(
                                    &state,
                                    &mut current_account_id,
                                    &mut request_headers_state,
                                    &mut pending_request,
                                    failure.clone(),
                                    failed_account_id,
                                    replacement,
                                )
                                .await;
                                continue;
                            }
                            PendingWebsocketRetryResult::NoReplacement { client_message } => {
                                if let Some(response) = deliver_pending_retry_failure_or_settle(
                                    &mut client_socket,
                                    &mut pending_request,
                                    client_message,
                                    &current_account_id,
                                    Some(failure.clone()),
                                )
                                .await
                                {
                                    return response;
                                }
                            }
                        }
                    }

                    return routed_websocket_outcome(&current_account_id, failure);
                }

                if sent_close {
                    return routed_websocket_outcome(&current_account_id, WebsocketProxyOutcome::Released);
                }
            }
            upstream_message = upstream_connection.stream.next() => {
                let message = match upstream_message {
                    Some(Ok(message)) => message,
                    Some(Err(error)) => {
                        let failure = WebsocketProxyOutcome::Failed {
                            failure: FailureClass::TemporaryFailure,
                            retry_after: None,
                            details: format!("responses websocket upstream stream failed: {error}"),
                        };

                        if pending_request
                            .as_ref()
                            .is_some_and(|pending| !pending.committed)
                        {
                            match retry_pending_websocket_request(
                                &state,
                                request_headers_state.headers,
                                &current_account_id,
                                &mut pending_request,
                            )
                            .await
                            {
                                PendingWebsocketRetryResult::Switched {
                                    failed_account_id,
                                    replacement,
                                } => {
                                    upstream_connection = apply_websocket_retry_switch(
                                        &state,
                                        &mut current_account_id,
                                        &mut request_headers_state,
                                        &mut pending_request,
                                        failure.clone(),
                                        failed_account_id,
                                        replacement,
                                    )
                                    .await;
                                    continue;
                                }
                                PendingWebsocketRetryResult::NoReplacement { client_message } => {
                                    if let Some(response) = deliver_pending_retry_failure_or_settle(
                                        &mut client_socket,
                                        &mut pending_request,
                                        client_message,
                                        &current_account_id,
                                        Some(failure.clone()),
                                    )
                                    .await
                                    {
                                        return response;
                                    }
                                }
                            }
                        }

                        return routed_websocket_outcome(&current_account_id, failure);
                    }
                    None => {
                        let failure = WebsocketProxyOutcome::Failed {
                            failure: FailureClass::TemporaryFailure,
                            retry_after: None,
                            details: "responses websocket upstream stream failed: closed".to_string(),
                        };

                        if pending_request
                            .as_ref()
                            .is_some_and(|pending| !pending.committed)
                        {
                            match retry_pending_websocket_request(
                                &state,
                                request_headers_state.headers,
                                &current_account_id,
                                &mut pending_request,
                            )
                            .await
                            {
                                PendingWebsocketRetryResult::Switched {
                                    failed_account_id,
                                    replacement,
                                } => {
                                    upstream_connection = apply_websocket_retry_switch(
                                        &state,
                                        &mut current_account_id,
                                        &mut request_headers_state,
                                        &mut pending_request,
                                        failure.clone(),
                                        failed_account_id,
                                        replacement,
                                    )
                                    .await;
                                    continue;
                                }
                                PendingWebsocketRetryResult::NoReplacement { client_message } => {
                                    if let Some(response) = deliver_pending_retry_failure_or_settle(
                                        &mut client_socket,
                                        &mut pending_request,
                                        client_message,
                                        &current_account_id,
                                        Some(failure.clone()),
                                    )
                                    .await
                                    {
                                        return response;
                                    }
                                }
                            }
                        }

                        return routed_websocket_outcome(&current_account_id, failure);
                    }
                };
                let Some(message) = normalize_websocket_rate_limit_message_impl(
                    message,
                    codex_originator,
                ) else {
                    continue;
                };
                // Preserve Codex's websocket reset behavior when upstream no longer
                // recognizes previous_response_id for an incremental create.
                let message = rewrite_previous_response_not_found_message_impl(message);
                let known_settlement = active_request_close_failure(
                    pending_request.as_ref(),
                    &message,
                    known_settlement_from_upstream_message(&message),
                );
                let classified_terminal_failure =
                    classify_websocket_upstream_terminal_failure_impl(&message);

                if should_passthrough_retryable_websocket_reset_impl(
                    pending_request
                        .as_ref()
                        .is_some_and(|pending| !pending.committed),
                    &message,
                ) {
                    let reset_settlement = None;
                    if let Some(response) = flush_buffered_or_settle(
                        &mut client_socket,
                        &mut pending_request,
                        &current_account_id,
                        reset_settlement.clone(),
                    )
                    .await
                    {
                        return response;
                    }

                    if let Some(response) = forward_message_or_settle(
                        &mut client_socket,
                        message,
                        &current_account_id,
                        reset_settlement.clone(),
                    )
                    .await
                    {
                        return response;
                    }

                    return routed_websocket_outcome(
                        &current_account_id,
                        outcome_after_client_delivery_failure(reset_settlement),
                    );
                }

                let precommit_failure = match (&pending_request, &known_settlement) {
                    (
                        Some(PendingWebsocketRequest {
                            committed: false, ..
                        }),
                        Some(failure @ WebsocketProxyOutcome::Failed { .. }),
                    ) => Some(failure.clone()),
                    _ => None,
                };
                if let Some(failure) = precommit_failure {
                    if should_failover_precommit_failure(
                        &failure,
                        classified_terminal_failure
                            .as_ref()
                            .map(|classified| (classified.status, classified.failure)),
                    ) {
                        match retry_pending_websocket_request(
                            &state,
                            request_headers_state.headers,
                            &current_account_id,
                            &mut pending_request,
                        )
                        .await
                        {
                            PendingWebsocketRetryResult::Switched {
                                failed_account_id,
                                replacement,
                            } => {
                                upstream_connection = apply_websocket_retry_switch(
                                    &state,
                                    &mut current_account_id,
                                    &mut request_headers_state,
                                    &mut pending_request,
                                    failure.clone(),
                                    failed_account_id,
                                    replacement,
                                )
                                .await;
                                continue;
                            }
                            PendingWebsocketRetryResult::NoReplacement { client_message } => {
                                if let Some(response) = deliver_pending_retry_failure_or_settle(
                                    &mut client_socket,
                                    &mut pending_request,
                                    client_message,
                                    &current_account_id,
                                    Some(failure.clone()),
                                )
                                .await
                                {
                                    return response;
                                }
                                return routed_websocket_outcome(&current_account_id, failure);
                            }
                        }
                    }

                    if let Some(response) = flush_buffered_or_settle(
                        &mut client_socket,
                        &mut pending_request,
                        &current_account_id,
                        Some(failure.clone()),
                    )
                    .await
                    {
                        return response;
                    }

                    if let Some(response) = forward_message_or_settle(
                        &mut client_socket,
                        message,
                        &current_account_id,
                        Some(failure.clone()),
                    )
                    .await
                    {
                        return response;
                    }

                    return routed_websocket_outcome(&current_account_id, failure);
                }

                if let Some(pending) = pending_request.as_mut()
                    && !pending.committed
                {
                    if should_buffer_upstream_message_before_commit(&message) {
                        pending.buffered_upstream_messages.push(message.clone());
                    } else if let Some(response) = forward_message_or_settle(
                        &mut client_socket,
                        message.clone(),
                        &current_account_id,
                        known_settlement.clone(),
                    )
                    .await
                    {
                        return response;
                    }

                    if upstream_message_commits_request_impl(&message) {
                        pending.committed = true;
                        if let Some(response) = flush_buffered_or_settle(
                            &mut client_socket,
                            &mut pending_request,
                            &current_account_id,
                            known_settlement.clone(),
                        )
                        .await
                        {
                            return response;
                        }
                    }

                    if upstream_message_is_terminal_impl(&message) {
                        pending_request = None;
                        if let Some(outcome) =
                            terminal_outcome_after_successful_delivery(known_settlement.clone())
                        {
                            return routed_websocket_outcome(
                                &current_account_id,
                                outcome,
                            );
                        }
                    }
                    continue;
                }

                if let Some(response) = forward_message_or_settle(
                    &mut client_socket,
                    message.clone(),
                    &current_account_id,
                    known_settlement.clone(),
                )
                .await
                {
                    return response;
                }

                if let Some(outcome) =
                    terminal_outcome_after_successful_delivery(known_settlement.clone())
                {
                    return routed_websocket_outcome(
                        &current_account_id,
                        outcome,
                    );
                }

                if upstream_message_is_terminal_impl(&message) {
                    pending_request = None;
                }
            }
        }
    }
}

pub(crate) async fn retry_pending_websocket_request(
    state: &AppState,
    request_headers: &HeaderMap,
    current_account_id: &str,
    pending_request: &mut Option<PendingWebsocketRequest>,
) -> PendingWebsocketRetryResult {
    let codex_originator = response_shape_for_headers(request_headers).is_codex();
    let Some(pending) = pending_request.as_mut() else {
        return PendingWebsocketRetryResult::NoReplacement {
            client_message: None,
        };
    };
    let failed_account_id = current_account_id.to_string();
    pending.tried_accounts.insert(failed_account_id.clone());

    loop {
        let replacement = match connect_responses_websocket_with_failover(
            state,
            request_headers,
            &mut pending.tried_accounts,
        )
        .await
        {
            Ok(replacement) => replacement,
            Err(failure) => {
                return PendingWebsocketRetryResult::NoReplacement {
                    client_message: websocket_error_message_for_failover_failure(&failure),
                };
            }
        };

        let RoutedExecution {
            account_id,
            mut value,
        } = replacement;

        let replay = replay_buffered_request_messages(
            &mut value.stream,
            &pending.request_messages,
            codex_originator,
            state.models_cache.read().await.current(),
            state.config.fingerprint_mode,
            value.installation_id.as_deref(),
        )
        .await;
        match replay {
            Ok(()) => {
                return PendingWebsocketRetryResult::Switched {
                    failed_account_id,
                    replacement: Box::new(RoutedExecution { account_id, value }),
                };
            }
            Err(error) => {
                pending.tried_accounts.insert(account_id.clone());
                apply_websocket_account_failure(
                    state,
                    &account_id,
                    FailureClass::TemporaryFailure,
                    None,
                    format!("responses websocket upstream stream failed: {error}"),
                )
                .await;
            }
        }
    }
}

async fn apply_websocket_retry_switch(
    state: &AppState,
    current_account_id: &mut String,
    request_headers_state: &mut RequestHeadersState<'_>,
    pending_request: &mut Option<PendingWebsocketRequest>,
    failure_outcome: WebsocketProxyOutcome,
    failed_account_id: String,
    replacement: Box<RoutedExecution<UpstreamWebsocketConnection>>,
) -> UpstreamWebsocketConnection {
    let replacement = *replacement;
    let WebsocketProxyOutcome::Failed {
        failure,
        retry_after,
        details,
    } = failure_outcome
    else {
        return replacement.value;
    };

    apply_websocket_account_failure(state, &failed_account_id, failure, retry_after, details).await;
    if let Some(pending) = pending_request.as_mut() {
        pending.buffered_upstream_messages.clear();
    }
    *current_account_id = replacement.account_id;
    apply_turn_state_to_request_headers(
        request_headers_state.headers,
        replacement.value.turn_state.as_deref(),
        request_headers_state.allow_upstream_turn_state,
    );
    replacement.value
}

fn apply_turn_state_to_request_headers(
    headers: &mut HeaderMap,
    turn_state: Option<&str>,
    allow_upstream_turn_state: bool,
) {
    if !allow_upstream_turn_state {
        return;
    }
    let Some(turn_state) = turn_state else {
        return;
    };
    if let Ok(value) = http::HeaderValue::from_str(turn_state) {
        headers.insert("x-codex-turn-state", value);
    }
}

async fn apply_websocket_account_failure(
    state: &AppState,
    account_id: &str,
    failure: FailureClass,
    retry_after: Option<Duration>,
    details: String,
) {
    let mut accounts = state.accounts.write().await;
    if let Err(error) = apply_account_settlement(
        &mut accounts,
        account_id,
        AccountSettlement::Failure {
            failure,
            retry_after,
            details,
        },
    ) {
        warn!(account_id = %account_id, %error, "failed to settle websocket account failure");
    }
}

async fn replay_buffered_request_messages(
    upstream_stream: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    request_messages: &[TungsteniteMessage],
    codex_originator: bool,
    snapshot: Option<std::sync::Arc<crate::models::ModelsSnapshot>>,
    mode: crate::config::FingerprintMode,
    installation_id: Option<&str>,
) -> Result<(), tokio_tungstenite::tungstenite::Error> {
    for message in request_messages {
        let message = if is_responses_websocket_request_start_impl(message) {
            normalize_response_create_message(
                message.clone(),
                mode,
                codex_originator,
                snapshot.as_deref(),
                installation_id,
            )
        } else {
            message.clone()
        };
        upstream_stream.send(message).await?;
    }
    Ok(())
}

async fn flush_buffered_upstream_messages(
    client_socket: &mut WebSocket,
    messages: Vec<TungsteniteMessage>,
) -> bool {
    for message in messages {
        if !forward_upstream_message(client_socket, message).await {
            return false;
        }
    }
    true
}

fn take_pending_buffered_upstream_messages(
    pending_request: &mut Option<PendingWebsocketRequest>,
) -> Vec<TungsteniteMessage> {
    pending_request
        .as_mut()
        .map(|pending| std::mem::take(&mut pending.buffered_upstream_messages))
        .unwrap_or_default()
}

async fn forward_upstream_message(
    client_socket: &mut WebSocket,
    message: TungsteniteMessage,
) -> bool {
    let Some(mapped) = map_upstream_message_to_client(message) else {
        return true;
    };
    client_socket.send(mapped).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outcome_after_client_delivery_failure_preserves_failed_outcome() {
        let failure = WebsocketProxyOutcome::Failed {
            failure: FailureClass::QuotaExhausted,
            retry_after: Some(Duration::from_secs(77)),
            details: "The usage limit has been reached".to_string(),
        };

        assert_eq!(
            outcome_after_client_delivery_failure(Some(failure.clone())),
            failure
        );
    }

    #[test]
    fn outcome_after_client_delivery_failure_preserves_success_outcome() {
        assert_eq!(
            outcome_after_client_delivery_failure(Some(WebsocketProxyOutcome::Success)),
            WebsocketProxyOutcome::Success
        );
    }

    #[test]
    fn terminal_outcome_after_successful_delivery_preserves_success() {
        assert_eq!(
            terminal_outcome_after_successful_delivery(Some(WebsocketProxyOutcome::Success)),
            Some(WebsocketProxyOutcome::Success)
        );
    }

    #[test]
    fn terminal_outcome_after_successful_delivery_preserves_failure() {
        let failure = WebsocketProxyOutcome::Failed {
            failure: FailureClass::QuotaExhausted,
            retry_after: Some(Duration::from_secs(77)),
            details: "The usage limit has been reached".to_string(),
        };

        assert_eq!(
            terminal_outcome_after_successful_delivery(Some(failure.clone())),
            Some(failure)
        );
    }

    #[test]
    fn terminal_outcome_after_successful_delivery_preserves_release_and_none() {
        assert_eq!(
            terminal_outcome_after_successful_delivery(Some(WebsocketProxyOutcome::Released)),
            Some(WebsocketProxyOutcome::Released)
        );
        assert_eq!(terminal_outcome_after_successful_delivery(None), None);
    }

    #[test]
    fn retryable_websocket_reset_passthrough_branch_releases_account() {
        let message = rewrite_previous_response_not_found_message_impl(TungsteniteMessage::Text(
            r#"{"type":"error","error":{"type":"invalid_request_error","code":"previous_response_not_found","message":"Previous response with id 'resp_123' not found.","param":"previous_response_id"},"status":400}"#
                .into(),
        ));

        assert!(should_passthrough_retryable_websocket_reset_impl(
            true, &message
        ));
        assert_eq!(
            outcome_after_client_delivery_failure(None),
            WebsocketProxyOutcome::Released
        );
    }

    #[test]
    fn response_completed_produces_known_success_settlement() {
        let message = TungsteniteMessage::Text(
            r#"{"type":"response.completed","response":{"id":"resp_done"}}"#.into(),
        );

        assert_eq!(
            known_settlement_from_upstream_message(&message),
            Some(WebsocketProxyOutcome::Success)
        );
    }

    #[test]
    fn active_request_close_none_becomes_temporary_failure() {
        let outcome = active_request_close_failure(
            Some(&PendingWebsocketRequest::default()),
            &TungsteniteMessage::Close(None),
            Some(WebsocketProxyOutcome::Released),
        );

        assert_eq!(
            outcome,
            Some(WebsocketProxyOutcome::Failed {
                failure: FailureClass::TemporaryFailure,
                retry_after: None,
                details: "responses websocket upstream stream failed: closed".to_string(),
            })
        );
    }

    #[test]
    fn active_request_normal_close_becomes_temporary_failure() {
        let outcome = active_request_close_failure(
            Some(&PendingWebsocketRequest::default()),
            &TungsteniteMessage::Close(Some(
                tokio_tungstenite::tungstenite::protocol::CloseFrame {
                    code:
                        tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode::Normal,
                    reason: "".into(),
                },
            )),
            Some(WebsocketProxyOutcome::Released),
        );

        assert_eq!(
            outcome,
            Some(WebsocketProxyOutcome::Failed {
                failure: FailureClass::TemporaryFailure,
                retry_after: None,
                details:
                    "responses websocket upstream closed before terminal response: code=1000 reason="
                        .to_string(),
            })
        );
    }

    #[test]
    fn idle_close_none_remains_released() {
        assert_eq!(
            active_request_close_failure(
                None,
                &TungsteniteMessage::Close(None),
                Some(WebsocketProxyOutcome::Released),
            ),
            Some(WebsocketProxyOutcome::Released)
        );
    }

    #[test]
    fn apply_turn_state_to_request_headers_preserves_existing_value_when_upstream_has_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-turn-state",
            http::HeaderValue::from_static("existing-turn-state"),
        );

        apply_turn_state_to_request_headers(&mut headers, None, true);

        assert_eq!(
            headers
                .get("x-codex-turn-state")
                .and_then(|value| value.to_str().ok()),
            Some("existing-turn-state")
        );
    }

    #[test]
    fn apply_turn_state_to_request_headers_ignores_upstream_value_when_disabled() {
        let mut headers = HeaderMap::new();

        apply_turn_state_to_request_headers(&mut headers, Some("upstream-turn-state"), false);

        assert!(headers.get("x-codex-turn-state").is_none());
    }

    #[test]
    fn wrapped_error_produces_known_failed_settlement() {
        let message = TungsteniteMessage::Text(
            r#"{"type":"error","status":401,"error":{"type":"authentication_error","code":"invalid_api_key","message":"bad key"}}"#
                .into(),
        );

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            details,
        }) = known_settlement_from_upstream_message(&message)
        else {
            panic!("expected failed settlement");
        };

        assert_eq!(failure, FailureClass::AccessTokenRejected);
        assert_eq!(retry_after, None);
        assert!(details.contains("status=401"));
        assert!(details.contains("error.code=invalid_api_key"));
    }

    #[test]
    fn response_failed_produces_known_failed_settlement() {
        let message = TungsteniteMessage::Text(
            r#"{"type":"response.failed","response":{"error":{"type":"permission_error","code":"forbidden","message":"forbidden"}}}"#
                .into(),
        );

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            details,
        }) = known_settlement_from_upstream_message(&message)
        else {
            panic!("expected failed settlement");
        };

        assert_eq!(failure, FailureClass::TemporaryFailure);
        assert_eq!(retry_after, None);
        assert_eq!(
            details,
            "responses websocket upstream returned response.failed: forbidden"
        );
    }

    #[test]
    fn precommit_forbidden_failure_retries_via_gateway_unavailable_status() {
        let message = TungsteniteMessage::Text(
            r#"{"type":"response.failed","response":{"error":{"type":"permission_error","code":"forbidden","message":"forbidden"}}}"#
                .into(),
        );
        let outcome = known_settlement_from_upstream_message(&message).expect("known settlement");
        let classified =
            classify_websocket_upstream_terminal_failure_impl(&message).expect("classified");

        assert!(should_failover_precommit_failure(
            &outcome,
            Some((classified.status, classified.failure)),
        ));
    }

    #[test]
    fn precommit_invalid_prompt_failure_does_not_retry() {
        let message = TungsteniteMessage::Text(
            r#"{"type":"response.failed","response":{"error":{"type":"invalid_request_error","code":"invalid_prompt","message":"bad prompt"}}}"#
                .into(),
        );
        let outcome = known_settlement_from_upstream_message(&message).expect("known settlement");
        let classified =
            classify_websocket_upstream_terminal_failure_impl(&message).expect("classified");

        assert!(!should_failover_precommit_failure(
            &outcome,
            Some((classified.status, classified.failure)),
        ));
    }
}
