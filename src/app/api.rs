use super::AppState;
use crate::failover::{
    AccountSettlement, FailoverFailure, SuccessDisposition, apply_account_settlement,
    connect_responses_websocket_with_failover, execute_unary_json_with_failover,
    execute_with_failover, spawn_account_settlement,
};
use crate::gateway_errors::json_error;
use crate::models::{ModelsResponseShape, ModelsSnapshot, response_shape_for_headers};
use crate::request_normalization::{
    apply_body_gateway_overrides, normalize_compact_request_body, normalize_responses_request_body,
};
use crate::responses::responses_pre_stream_failure_response;
use crate::responses::{ManagedResponseStream, WebsocketProxyOutcome, proxy_websocket};
use axum::Json;
use axum::body::Body;
use axum::body::to_bytes;
use axum::extract::{OriginalUri, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::warn;

pub(crate) async fn get_models(
    State(state): State<AppState>,
    _uri: OriginalUri,
    headers: HeaderMap,
) -> Response {
    match response_shape_for_headers(&headers) {
        ModelsResponseShape::Codex => match fetch_models_snapshot(&state, &headers).await {
            Ok(snapshot) => Json(snapshot.codex_json()).into_response(),
            Err(error) => error.into_response(),
        },
        ModelsResponseShape::OpenAi => {
            if let Some(snapshot) = cached_or_refreshing_models_snapshot(&state, &headers).await {
                Json(snapshot.openai_json()).into_response()
            } else {
                match fetch_models_snapshot(&state, &headers).await {
                    Ok(snapshot) => Json(snapshot.openai_json()).into_response(),
                    Err(error) => error.into_response(),
                }
            }
        }
    }
}

pub(crate) async fn get_responses_websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    _uri: OriginalUri,
    headers: HeaderMap,
) -> Response {
    let upgrade_request_headers = headers.clone();
    let mut tried_accounts = HashSet::new();
    let routed = match connect_responses_websocket_with_failover(
        &state,
        &headers,
        &mut tried_accounts,
    )
    .await
    {
        Ok(upstream) => upstream,
        Err(error) => return error.into_response(),
    };

    let release_state = state.clone();
    let failed_upgrade_state = state.clone();
    let failed_upgrade_account_id = routed.account_id.clone();
    ws.on_failed_upgrade(move |_error| {
        let failed_upgrade_state = failed_upgrade_state.clone();
        let failed_upgrade_account_id = failed_upgrade_account_id.clone();
        tokio::spawn(async move {
            let mut accounts = failed_upgrade_state.accounts.write().await;
            if let Err(error) = apply_account_settlement(
                &mut accounts,
                &failed_upgrade_account_id,
                AccountSettlement::Release,
            ) {
                warn!(
                    account_id = %failed_upgrade_account_id,
                    %error,
                    "failed to settle account state after websocket upgrade failure"
                );
            }
        });
    })
    .on_upgrade(move |socket| async move {
        let routed_outcome = proxy_websocket(
            socket,
            release_state.clone(),
            upgrade_request_headers,
            routed,
        )
        .await;
        let settlement = match routed_outcome.value {
            WebsocketProxyOutcome::Success => AccountSettlement::Success,
            WebsocketProxyOutcome::Released => AccountSettlement::Release,
            WebsocketProxyOutcome::Failed {
                failure,
                retry_after,
                details,
            } => AccountSettlement::Failure {
                failure,
                retry_after,
                details,
            },
        };
        spawn_account_settlement(release_state, routed_outcome.account_id, settlement);
    })
    .into_response()
}

pub(crate) async fn post_responses(
    State(state): State<AppState>,
    _uri: OriginalUri,
    mut headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let models_snapshot = state.models_cache.read().await.current();
    normalize_responses_request_body(
        state.config.fingerprint_mode,
        &mut body,
        models_snapshot.as_deref(),
    );
    if let Err(error) = apply_body_gateway_overrides(&mut headers, &mut body) {
        return json_error(StatusCode::BAD_REQUEST, error);
    }
    let request_headers = headers.clone();
    let request_body = body.clone();
    let upstream_client = state.upstream.clone();
    let codex_originator = matches!(
        response_shape_for_headers(&headers),
        ModelsResponseShape::Codex
    );
    let upstream = match execute_with_failover(
        &state,
        SuccessDisposition::HoldUntilCaller,
        move |upstream_account| {
            let request_headers = request_headers.clone();
            let request_body = request_body.clone();
            let upstream_client = upstream_client.clone();
            async move {
                upstream_client
                    .stream_json(
                        "responses",
                        &upstream_account,
                        &request_headers,
                        request_body,
                    )
                    .await
            }
        },
    )
    .await
    {
        Ok(upstream) => upstream,
        Err(error) => return responses_pre_stream_failure_response(&error, codex_originator),
    };

    let stream = ManagedResponseStream::new(state, upstream.account_id, upstream.value.bytes);
    (
        upstream.value.status,
        upstream.value.headers,
        Body::from_stream(stream),
    )
        .into_response()
}

pub(crate) async fn post_responses_compact(
    State(state): State<AppState>,
    _uri: OriginalUri,
    mut headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    let models_snapshot = state.models_cache.read().await.current();
    normalize_compact_request_body(
        state.config.fingerprint_mode,
        &mut body,
        models_snapshot.as_deref(),
    );
    if let Err(error) = apply_body_gateway_overrides(&mut headers, &mut body) {
        return json_error(StatusCode::BAD_REQUEST, error);
    }
    let request_headers = headers.clone();
    let request_body = body.clone();
    let upstream_client = state.upstream.clone();
    match execute_unary_json_with_failover(&state, move |upstream_account| {
        let request_headers = request_headers.clone();
        let request_body = request_body.clone();
        let upstream_client = upstream_client.clone();
        async move {
            upstream_client
                .post_json(
                    "responses/compact",
                    &upstream_account,
                    &request_headers,
                    request_body,
                )
                .await
        }
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

pub(crate) async fn post_memories_trace_summarize(
    State(state): State<AppState>,
    _uri: OriginalUri,
    mut headers: HeaderMap,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(error) = apply_body_gateway_overrides(&mut headers, &mut body) {
        return json_error(StatusCode::BAD_REQUEST, error);
    }
    let request_headers = headers.clone();
    let request_body = body.clone();
    let upstream_client = state.upstream.clone();
    match execute_unary_json_with_failover(&state, move |upstream_account| {
        let request_headers = request_headers.clone();
        let request_body = request_body.clone();
        let upstream_client = upstream_client.clone();
        async move {
            upstream_client
                .post_json(
                    "memories/trace_summarize",
                    &upstream_account,
                    &request_headers,
                    request_body,
                )
                .await
        }
    })
    .await
    {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

async fn cached_or_refreshing_models_snapshot(
    state: &AppState,
    headers: &HeaderMap,
) -> Option<Arc<ModelsSnapshot>> {
    let (fresh, current) = {
        let cache = state.models_cache.read().await;
        (cache.fresh(), cache.current())
    };
    if fresh.is_some() {
        return fresh;
    }
    if let Some(snapshot) = current {
        maybe_spawn_models_refresh(state.clone(), headers.clone());
        return Some(snapshot);
    }
    None
}

fn maybe_spawn_models_refresh(state: AppState, headers: HeaderMap) {
    if state
        .models_refresh_in_flight
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        if let Err(error) = fetch_models_snapshot(&state, &headers).await {
            warn!(?error, "background models refresh failed");
        }
        state
            .models_refresh_in_flight
            .store(false, Ordering::Release);
    });
}

async fn fetch_models_snapshot(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Arc<ModelsSnapshot>, FailoverFailure> {
    let request_headers = headers.clone();
    let upstream_client = state.upstream.clone();
    let response = execute_unary_json_with_failover(state, move |upstream_account| {
        let request_headers = request_headers.clone();
        let upstream_client = upstream_client.clone();
        async move {
            upstream_client
                .get_models(&upstream_account, &request_headers)
                .await
        }
    })
    .await?;

    let body = response_body_bytes(response).await?;
    let json_body: Value =
        serde_json::from_slice(&body).map_err(|error| FailoverFailure::Json {
            status: StatusCode::BAD_GATEWAY,
            message: format!("decode models response json failed: {error}"),
        })?;
    let snapshot =
        Arc::new(
            ModelsSnapshot::from_value(json_body).map_err(|error| FailoverFailure::Json {
                status: StatusCode::BAD_GATEWAY,
                message: error.to_string(),
            })?,
        );
    state.models_cache.write().await.replace(snapshot.clone());
    Ok(snapshot)
}

async fn response_body_bytes(response: Response) -> Result<bytes::Bytes, FailoverFailure> {
    let (_, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX)
        .await
        .map_err(|error| FailoverFailure::Json {
            status: StatusCode::BAD_GATEWAY,
            message: format!("read models response body failed: {error}"),
        })?;
    Ok(bytes)
}
