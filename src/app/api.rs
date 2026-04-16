use super::AppState;
use crate::failover::{
    AccountSettlement, FailoverFailure, SuccessDisposition, apply_account_settlement,
    connect_responses_websocket_with_failover, execute_unary_json_with_failover,
    execute_unary_json_with_failover_shaped, execute_with_failover, execute_with_failover_from,
    spawn_account_settlement,
};
use crate::gateway_errors::json_error;
use crate::gateway_errors::{
    FailureRenderMode, render_failover_failure, render_status_message_error,
};
use crate::gateway_errors::{JSON_CONTENT_TYPE, SSE_CONTENT_TYPE, set_content_type};
use crate::http_shape::shape_openai_http_headers;
use crate::models::{ModelsSnapshot, ResponseShape, response_shape_for_headers};
use crate::request_normalization::{
    apply_body_gateway_overrides, normalize_compact_request_body, normalize_responses_request_body,
};
use crate::responses::{ManagedResponseStream, WebsocketProxyOutcome, proxy_websocket};
use crate::upstream::body_as_json;
use axum::Json;
use axum::body::Body;
#[cfg(test)]
use axum::body::to_bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{OriginalUri, State, WebSocketUpgrade};
use axum::http::header::CACHE_CONTROL;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tracing::warn;

pub(crate) struct PreparedJsonRequest {
    pub(crate) failure_context: FailureContext,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Value,
}

struct FetchedModelsSnapshot {
    snapshot: Arc<ModelsSnapshot>,
    response_headers: HeaderMap,
}

#[derive(Debug)]
pub(crate) struct PreparedRequestError {
    pub(crate) failure_context: FailureContext,
    pub(crate) status: StatusCode,
    pub(crate) message: String,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct FailureContext {
    pub(crate) response_shape: ResponseShape,
    pub(crate) render_mode: FailureRenderMode,
}

impl FailureContext {
    fn for_headers(headers: &HeaderMap, render_mode: FailureRenderMode) -> Self {
        Self {
            response_shape: response_shape_for_headers(headers),
            render_mode,
        }
    }
}

impl PreparedRequestError {
    fn into_response(self) -> Response {
        render_status_message_error(
            self.failure_context.response_shape,
            self.failure_context.render_mode,
            self.status,
            self.message,
        )
    }
}

pub(crate) async fn get_models(
    State(state): State<AppState>,
    _uri: OriginalUri,
    headers: HeaderMap,
) -> Response {
    let failure_context = FailureContext::for_headers(&headers, FailureRenderMode::UnaryJson);
    match failure_context.response_shape {
        ResponseShape::Codex => match fetch_models_snapshot(&state, &headers).await {
            Ok(fetched) => Json(fetched.snapshot.codex_json()).into_response(),
            Err(error) => render_failover_failure(
                &error,
                failure_context.response_shape,
                failure_context.render_mode,
            ),
        },
        ResponseShape::OpenAi => {
            if let Some((snapshot, response_headers)) =
                cached_or_refreshing_models_snapshot(&state, &headers).await
            {
                models_openai_success_response(snapshot, response_headers)
            } else {
                match fetch_models_snapshot(&state, &headers).await {
                    Ok(fetched) => {
                        models_openai_success_response(fetched.snapshot, fetched.response_headers)
                    }
                    Err(error) => render_failover_failure(
                        &error,
                        failure_context.response_shape,
                        failure_context.render_mode,
                    ),
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
    let failure_context = FailureContext::for_headers(&headers, FailureRenderMode::UnaryJson);
    let propagate_turn_state = failure_context.response_shape.is_codex();
    let mut upgrade_request_headers = headers.clone();
    let mut tried_accounts = HashSet::new();
    let routed = match connect_responses_websocket_with_failover(
        &state,
        &headers,
        &mut tried_accounts,
    )
    .await
    {
        Ok(upstream) => upstream,
        Err(error) => {
            return render_failover_failure(
                &error,
                failure_context.response_shape,
                failure_context.render_mode,
            );
        }
    };
    let routed_turn_state = routed.value.turn_state.clone();
    if propagate_turn_state {
        apply_turn_state_header(&mut upgrade_request_headers, routed_turn_state.as_deref());
    }

    let release_state = state.clone();
    let failed_upgrade_state = state.clone();
    let failed_upgrade_account_id = routed.account_id.clone();
    let mut response = ws
        .on_failed_upgrade(move |_error| {
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
                propagate_turn_state,
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
        .into_response();
    if propagate_turn_state {
        apply_turn_state_header(response.headers_mut(), routed_turn_state.as_deref());
    }
    response
}

pub(crate) async fn post_responses(
    State(state): State<AppState>,
    _uri: OriginalUri,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let prepared = match prepare_responses_request(&state, headers, body).await {
        Ok(prepared) => prepared,
        Err(error) => return error.into_response(),
    };
    let request_headers = prepared.headers.clone();
    let request_body = prepared.body.clone();
    let upstream_client = state.upstream.clone();
    if should_stream_responses_request(&prepared.body) {
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
            Err(error) => {
                return render_failover_failure(
                    &error,
                    prepared.failure_context.response_shape,
                    prepared.failure_context.render_mode,
                );
            }
        };

        let response_headers = shape_responses_success_headers(
            prepared.failure_context.response_shape,
            &prepared.body,
            upstream.value.headers,
        );
        let stream = ManagedResponseStream::new(
            state,
            upstream.account_id,
            upstream.value.bytes,
            prepared.failure_context.response_shape,
        );
        return (
            upstream.value.status,
            response_headers,
            Body::from_stream(stream),
        )
            .into_response();
    }

    match execute_unary_json_with_failover_shaped(
        &state,
        move |upstream_account| {
            let request_headers = request_headers.clone();
            let request_body = request_body.clone();
            let upstream_client = upstream_client.clone();
            async move {
                upstream_client
                    .post_responses_json(&upstream_account, &request_headers, request_body)
                    .await
            }
        },
        move |status, response_headers, json_body| {
            let response_headers = shape_unary_responses_success_headers(
                prepared.failure_context.response_shape,
                response_headers,
            );
            (status, response_headers, Json(json_body)).into_response()
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => render_failover_failure(
            &error,
            prepared.failure_context.response_shape,
            prepared.failure_context.render_mode,
        ),
    }
}

fn should_stream_responses_request(body: &Value) -> bool {
    !matches!(body.get("stream"), Some(Value::Bool(false)))
}

fn shape_responses_success_headers(
    response_shape: ResponseShape,
    request_body: &Value,
    response_headers: HeaderMap,
) -> HeaderMap {
    let mut response_headers = shape_openai_success_headers(response_shape, response_headers);
    if response_shape == ResponseShape::OpenAi && should_stream_responses_request(request_body) {
        set_content_type(&mut response_headers, SSE_CONTENT_TYPE);
    }
    response_headers
}

fn shape_unary_responses_success_headers(
    response_shape: ResponseShape,
    response_headers: HeaderMap,
) -> HeaderMap {
    let mut response_headers = shape_openai_success_headers(response_shape, response_headers);
    if response_shape == ResponseShape::OpenAi {
        set_content_type(&mut response_headers, JSON_CONTENT_TYPE);
    }
    response_headers
}

fn shape_openai_success_headers(
    response_shape: ResponseShape,
    response_headers: HeaderMap,
) -> HeaderMap {
    if response_shape != ResponseShape::OpenAi {
        return response_headers;
    }

    shape_openai_http_headers(response_headers)
}

fn models_openai_success_response(
    snapshot: Arc<ModelsSnapshot>,
    response_headers: HeaderMap,
) -> Response {
    let mut response_headers =
        shape_openai_success_headers(ResponseShape::OpenAi, response_headers);
    set_content_type(&mut response_headers, JSON_CONTENT_TYPE);
    (response_headers, Json(snapshot.openai_json())).into_response()
}

fn apply_turn_state_header(headers: &mut HeaderMap, turn_state: Option<&str>) {
    let Some(turn_state) = turn_state else {
        return;
    };
    if let Ok(value) = HeaderValue::from_str(turn_state) {
        headers.insert("x-codex-turn-state", value);
    }
}

pub(crate) async fn post_responses_compact(
    State(state): State<AppState>,
    _uri: OriginalUri,
    headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Response {
    let prepared = match prepare_compact_request(&state, headers, body).await {
        Ok(prepared) => prepared,
        Err(error) => return error.into_response(),
    };
    let request_headers = prepared.headers.clone();
    let request_body = prepared.body.clone();
    let upstream_client = state.upstream.clone();
    match execute_unary_json_with_failover_shaped(
        &state,
        move |upstream_account| {
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
        },
        move |status, response_headers, json_body| {
            let (response_headers, json_body) = shape_compact_success_response(
                prepared.failure_context.response_shape,
                response_headers,
                json_body,
            );
            (status, response_headers, Json(json_body)).into_response()
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => render_failover_failure(
            &error,
            prepared.failure_context.response_shape,
            prepared.failure_context.render_mode,
        ),
    }
}

pub(crate) async fn prepare_responses_request(
    state: &AppState,
    mut headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<PreparedJsonRequest, PreparedRequestError> {
    let default_failure_context =
        FailureContext::for_headers(&headers, FailureRenderMode::ResponsesPreStream);
    let codex_originator = default_failure_context.response_shape.is_codex();
    let mut body = extract_json_request_body(default_failure_context, body)?;
    let failure_context = FailureContext {
        response_shape: default_failure_context.response_shape,
        render_mode: if should_stream_responses_request(&body) {
            FailureRenderMode::ResponsesPreStream
        } else {
            FailureRenderMode::UnaryJson
        },
    };
    let models_snapshot = state.models_cache.read().await.current();
    normalize_responses_request_body(
        state.config.fingerprint_mode,
        codex_originator,
        &mut body,
        models_snapshot.as_deref(),
    );
    if let Err(error) = apply_body_gateway_overrides(&mut headers, &mut body) {
        return Err(PreparedRequestError {
            failure_context,
            status: StatusCode::BAD_REQUEST,
            message: error,
        });
    }
    Ok(PreparedJsonRequest {
        failure_context,
        headers,
        body,
    })
}

pub(crate) async fn prepare_compact_request(
    state: &AppState,
    mut headers: HeaderMap,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<PreparedJsonRequest, PreparedRequestError> {
    let failure_context = FailureContext::for_headers(&headers, FailureRenderMode::UnaryJson);
    let mut body = extract_json_request_body(failure_context, body)?;
    let models_snapshot = state.models_cache.read().await.current();
    normalize_compact_request_body(
        state.config.fingerprint_mode,
        failure_context.response_shape.is_codex(),
        &mut body,
        models_snapshot.as_deref(),
    );
    if let Err(error) = apply_body_gateway_overrides(&mut headers, &mut body) {
        return Err(PreparedRequestError {
            failure_context,
            status: StatusCode::BAD_REQUEST,
            message: error,
        });
    }
    Ok(PreparedJsonRequest {
        failure_context,
        headers,
        body,
    })
}

fn extract_json_request_body(
    failure_context: FailureContext,
    body: Result<Json<Value>, JsonRejection>,
) -> Result<Value, PreparedRequestError> {
    match body {
        Ok(Json(body)) => Ok(body),
        Err(rejection) => Err(PreparedRequestError {
            failure_context,
            status: rejection.status(),
            message: rejection.body_text(),
        }),
    }
}

fn rewrite_openai_compact_response_body(body: &mut Value) {
    let Some(output) = body.get_mut("output").and_then(Value::as_array_mut) else {
        return;
    };

    for item in output {
        if item.get("type").and_then(Value::as_str) != Some("compaction_summary") {
            continue;
        }
        let Some(object) = item.as_object_mut() else {
            continue;
        };
        object.insert("type".to_string(), Value::String("compaction".to_string()));
    }
}

fn shape_compact_success_response(
    response_shape: ResponseShape,
    response_headers: HeaderMap,
    mut json_body: Value,
) -> (HeaderMap, Value) {
    let mut response_headers = shape_openai_success_headers(response_shape, response_headers);
    if response_shape == ResponseShape::OpenAi {
        set_content_type(&mut response_headers, JSON_CONTENT_TYPE);
        rewrite_openai_compact_response_body(&mut json_body);
    }
    (response_headers, json_body)
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
) -> Option<(Arc<ModelsSnapshot>, HeaderMap)> {
    let (fresh, current) = {
        let cache = state.models_cache.read().await;
        (cache.fresh_entry(), cache.current_entry())
    };
    if fresh.is_some() {
        return fresh;
    }
    if let Some(entry) = current {
        maybe_spawn_models_refresh(state.clone(), headers.clone());
        return Some(stale_models_cached_entry(entry));
    }
    None
}

fn stale_models_cached_entry(
    (snapshot, mut response_headers): (Arc<ModelsSnapshot>, HeaderMap),
) -> (Arc<ModelsSnapshot>, HeaderMap) {
    response_headers.remove(CACHE_CONTROL);
    response_headers.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (snapshot, response_headers)
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
) -> Result<FetchedModelsSnapshot, FailoverFailure> {
    let request_headers = headers.clone();
    let upstream_client = state.upstream.clone();
    let mut tried_accounts = HashSet::new();
    let mut parse_failed_accounts = HashSet::new();
    let mut last_parse_failure = None;

    loop {
        let request_headers = request_headers.clone();
        let upstream_client = upstream_client.clone();
        let upstream = match execute_with_failover_from(
            state,
            SuccessDisposition::HoldUntilCaller,
            &mut tried_accounts,
            move |upstream_account| {
                let request_headers = request_headers.clone();
                let upstream_client = upstream_client.clone();
                async move {
                    upstream_client
                        .get_models(&upstream_account, &request_headers)
                        .await
                }
            },
        )
        .await
        {
            Ok(upstream) => upstream,
            Err(error) => {
                if matches!(error, FailoverFailure::PoolBlocked(_))
                    && !parse_failed_accounts.is_empty()
                    && parse_failed_accounts == tried_accounts
                    && let Some(detail) = last_parse_failure
                {
                    return Err(FailoverFailure::Internal {
                        status: StatusCode::BAD_GATEWAY,
                        detail,
                    });
                }
                return Err(error);
            }
        };

        let account_id = upstream.account_id.clone();
        let response_headers = upstream.value.headers.clone();
        let json_body = match body_as_json(&upstream.value.body) {
            Ok(json_body) => json_body,
            Err(error) => {
                settle_models_parse_failure(
                    state,
                    &account_id,
                    &error.to_string(),
                    &mut tried_accounts,
                    &mut parse_failed_accounts,
                    &mut last_parse_failure,
                )
                .await?;
                continue;
            }
        };

        let snapshot = match ModelsSnapshot::from_value(json_body) {
            Ok(snapshot) => Arc::new(snapshot),
            Err(error) => {
                settle_models_parse_failure(
                    state,
                    &account_id,
                    &error.to_string(),
                    &mut tried_accounts,
                    &mut parse_failed_accounts,
                    &mut last_parse_failure,
                )
                .await?;
                continue;
            }
        };

        {
            let mut accounts = state.accounts.write().await;
            if let Err(error) =
                apply_account_settlement(&mut accounts, &account_id, AccountSettlement::Success)
            {
                return Err(FailoverFailure::Internal {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    detail: error.to_string(),
                });
            }
        }
        state
            .models_cache
            .write()
            .await
            .replace(snapshot.clone(), response_headers.clone());
        return Ok(FetchedModelsSnapshot {
            snapshot,
            response_headers,
        });
    }
}

async fn settle_models_parse_failure(
    state: &AppState,
    account_id: &str,
    detail: &str,
    tried_accounts: &mut HashSet<String>,
    parse_failed_accounts: &mut HashSet<String>,
    last_parse_failure: &mut Option<String>,
) -> Result<(), FailoverFailure> {
    let details = detail.to_string();
    let mut accounts = state.accounts.write().await;
    if let Err(error) = apply_account_settlement(
        &mut accounts,
        account_id,
        AccountSettlement::Failure {
            failure: crate::classifier::FailureClass::TemporaryFailure,
            retry_after: None,
            details: details.clone(),
        },
    ) {
        return Err(FailoverFailure::Internal {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            detail: error.to_string(),
        });
    }
    tried_accounts.insert(account_id.to_string());
    parse_failed_accounts.insert(account_id.to_string());
    *last_parse_failure = Some(details);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;

    #[test]
    fn rewrite_openai_compact_response_body_rewrites_compaction_summary_items() {
        let mut body = json!({
            "object": "response.compaction",
            "output": [
                {
                    "type": "message",
                    "role": "user",
                    "content": [{ "type": "input_text", "text": "hi" }]
                },
                {
                    "type": "compaction_summary",
                    "encrypted_content": "abc"
                }
            ]
        });

        rewrite_openai_compact_response_body(&mut body);

        assert_eq!(
            body.pointer("/output/0/type").and_then(Value::as_str),
            Some("message")
        );
        assert_eq!(
            body.pointer("/output/1/type").and_then(Value::as_str),
            Some("compaction")
        );
        assert_eq!(
            body.pointer("/output/1/encrypted_content")
                .and_then(Value::as_str),
            Some("abc")
        );
    }

    #[test]
    fn rewrite_openai_compact_response_body_is_noop_without_compaction_summary() {
        let mut body = json!({
            "object": "response.compaction",
            "output": [
                { "type": "message", "role": "user" }
            ]
        });

        rewrite_openai_compact_response_body(&mut body);

        assert_eq!(
            body.pointer("/output/0/type").and_then(Value::as_str),
            Some("message")
        );
    }

    #[tokio::test]
    async fn openai_compact_success_response_sets_json_content_type() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
        headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-123"));
        let body = json!({
            "object": "response.compaction",
            "output": [
                { "type": "compaction_summary", "encrypted_content": "abc" }
            ]
        });

        let (headers, body) = shape_compact_success_response(ResponseShape::OpenAi, headers, body);

        let response = (StatusCode::OK, headers, Json(body)).into_response();
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(
            parts
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            parts
                .headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(parts.headers.get("x-codex-turn-state").is_none());
        assert_eq!(
            json.pointer("/output/0/type").and_then(Value::as_str),
            Some("compaction")
        );
    }

    #[tokio::test]
    async fn models_openai_success_response_sets_json_content_type() {
        let snapshot = Arc::new(
            ModelsSnapshot::from_value(json!({
                "models": [{
                    "slug": "gpt-5.4",
                    "display_name": "GPT-5.4",
                    "description": null,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "supported_in_api": true,
                    "priority": 1,
                    "availability_nux": null,
                    "upgrade": null,
                    "base_instructions": "",
                    "model_messages": null,
                    "supports_reasoning_summaries": false,
                    "default_reasoning_summary": "auto",
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "web_search_tool_type": "text",
                    "truncation_policy": { "mode": "bytes", "limit": 10000 },
                    "supports_parallel_tool_calls": true,
                    "supports_image_detail_original": false,
                    "context_window": 272000,
                    "auto_compact_token_limit": null,
                    "effective_context_window_percent": 95,
                    "experimental_supported_tools": [],
                    "input_modalities": ["text"],
                    "used_fallback_model_metadata": false,
                    "supports_search_tool": false
                }]
            }))
            .expect("snapshot"),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        );
        headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-123"));

        let response = models_openai_success_response(snapshot, headers);
        let (parts, body) = response.into_parts();
        let bytes = to_bytes(body, usize::MAX).await.expect("body bytes");
        let json: Value = serde_json::from_slice(&bytes).expect("json");

        assert_eq!(
            parts
                .headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            parts
                .headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
        assert!(parts.headers.get("x-codex-turn-state").is_none());
        assert_eq!(
            json.pointer("/data/0/id").and_then(Value::as_str),
            Some("gpt-5.4")
        );
    }

    #[test]
    fn shape_responses_success_headers_keeps_json_when_openai_stream_is_false() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=0"),
        );
        headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-123"));
        headers.insert("traceparent", HeaderValue::from_static("00-abc-123-01"));

        let headers = shape_responses_success_headers(
            ResponseShape::OpenAi,
            &json!({
                "model": "gpt-5.4",
                "stream": false
            }),
            headers,
        );

        assert_eq!(
            headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some("application/json")
        );
        assert_eq!(
            headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private, max-age=0")
        );
        assert!(headers.get("x-codex-turn-state").is_none());
        assert!(headers.get("traceparent").is_none());
    }

    #[test]
    fn shape_responses_success_headers_forces_sse_when_openai_streams() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache"),
        );
        headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-123"));

        let headers = shape_responses_success_headers(
            ResponseShape::OpenAi,
            &json!({
                "model": "gpt-5.4",
                "stream": true
            }),
            headers,
        );

        assert_eq!(
            headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some(SSE_CONTENT_TYPE)
        );
        assert_eq!(
            headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-cache")
        );
        assert!(headers.get("x-codex-turn-state").is_none());
    }

    #[test]
    fn responses_streaming_defaults_to_true_when_stream_is_missing() {
        assert!(should_stream_responses_request(&json!({
            "model": "gpt-5.4"
        })));
    }

    #[test]
    fn responses_streaming_is_enabled_when_stream_is_true() {
        assert!(should_stream_responses_request(&json!({
            "model": "gpt-5.4",
            "stream": true
        })));
    }

    #[test]
    fn responses_streaming_is_disabled_when_stream_is_false() {
        assert!(!should_stream_responses_request(&json!({
            "model": "gpt-5.4",
            "stream": false
        })));
    }

    #[test]
    fn unary_responses_success_headers_force_json_for_openai() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; charset=utf-8"),
        );
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("private"),
        );
        headers.insert("x-codex-turn-state", HeaderValue::from_static("turn-123"));

        let headers = shape_unary_responses_success_headers(ResponseShape::OpenAi, headers);

        assert_eq!(
            headers
                .get("content-type")
                .and_then(|value| value.to_str().ok()),
            Some(JSON_CONTENT_TYPE)
        );
        assert_eq!(
            headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("private")
        );
        assert!(headers.get("x-codex-turn-state").is_none());
    }

    #[test]
    fn apply_turn_state_header_preserves_existing_value_when_upstream_has_none() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-codex-turn-state",
            HeaderValue::from_static("existing-turn-state"),
        );

        apply_turn_state_header(&mut headers, None);

        assert_eq!(
            headers
                .get("x-codex-turn-state")
                .and_then(|value| value.to_str().ok()),
            Some("existing-turn-state")
        );
    }

    #[test]
    fn stale_models_cached_entry_forces_no_store_cache_control() {
        let snapshot = Arc::new(
            ModelsSnapshot::from_value(json!({
                "models": [{
                    "slug": "gpt-5.4",
                    "display_name": "GPT-5.4",
                    "description": null,
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "supported_in_api": true,
                    "priority": 1,
                    "availability_nux": null,
                    "upgrade": null,
                    "base_instructions": "",
                    "model_messages": null,
                    "supports_reasoning_summaries": false,
                    "default_reasoning_summary": "auto",
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "web_search_tool_type": "text",
                    "truncation_policy": { "mode": "bytes", "limit": 10000 },
                    "supports_parallel_tool_calls": true,
                    "supports_image_detail_original": false,
                    "context_window": 272000,
                    "auto_compact_token_limit": null,
                    "effective_context_window_percent": 95,
                    "experimental_supported_tools": [],
                    "input_modalities": ["text"],
                    "used_fallback_model_metadata": false,
                    "supports_search_tool": false
                }]
            }))
            .expect("snapshot"),
        );
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::CACHE_CONTROL,
            HeaderValue::from_static("private, max-age=60"),
        );

        let (_snapshot, headers) = stale_models_cached_entry((snapshot, headers));

        assert_eq!(
            headers
                .get(axum::http::header::CACHE_CONTROL)
                .and_then(|value| value.to_str().ok()),
            Some("no-store")
        );
    }
}
