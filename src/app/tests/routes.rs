use super::*;
use crate::app::api::{
    get_models, post_responses, post_responses_compact, prepare_compact_request,
    prepare_responses_request,
};
use crate::app::public_routes;
use crate::upstream::fingerprint::stable_installation_id;
use axum::Json as AxumJson;
use axum::Router;
use axum::body::Body;
use axum::extract::{OriginalUri, State, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use bytes::Bytes;
use futures::stream;
use serde_json::{Value, json};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};
use tower::util::ServiceExt;

struct SeededTestState {
    _temp: TempDir,
    state: AppState,
}

struct SeededPairTestState {
    _temp: TempDir,
    state: AppState,
    account_ids: [String; 2],
}

async fn seeded_state_with_config(config: RuntimeConfig) -> SeededTestState {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(RuntimeConfig {
        accounts_dir: temp.path().to_path_buf(),
        ..config
    })
    .expect("state builds");
    {
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
                    access_token_expires_at: Some(
                        chrono::Utc::now() + chrono::Duration::minutes(30),
                    ),
                },
            )
            .expect("refresh success seeded");
    }
    SeededTestState { _temp: temp, state }
}

async fn seeded_state_pair_with_config(config: RuntimeConfig) -> SeededPairTestState {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(RuntimeConfig {
        accounts_dir: temp.path().to_path_buf(),
        ..config
    })
    .expect("state builds");
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
                    access_token_expires_at: Some(
                        chrono::Utc::now() + chrono::Duration::minutes(30),
                    ),
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
                    access_token_expires_at: Some(
                        chrono::Utc::now() + chrono::Duration::minutes(30),
                    ),
                },
            )
            .expect("second refresh success seeded");
        [first, second]
    };
    SeededPairTestState {
        _temp: temp,
        state,
        account_ids,
    }
}

async fn spawn_mock_responses_upstream(
    captured: Arc<Mutex<Option<(HeaderMap, Value)>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/responses",
        post({
            let captured = captured.clone();
            move |headers: HeaderMap, AxumJson(body): AxumJson<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some((headers, body));
                    let mut response_headers = HeaderMap::new();
                    response_headers.insert("cache-control", HeaderValue::from_static("no-store"));
                    response_headers.insert(
                        "x-codex-turn-state",
                        HeaderValue::from_static("turn-state-123"),
                    );
                    response_headers
                        .insert("traceparent", HeaderValue::from_static("00-abc-123-01"));
                    (
                        response_headers,
                        AxumJson(json!({
                            "id": "resp_test",
                            "object": "response",
                            "status": "completed",
                            "output": []
                        })),
                    )
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock upstream");
    });
    (format!("http://{addr}"), handle)
}

async fn spawn_mock_responses_stream_upstream(
    captured: Arc<Mutex<Option<HeaderMap>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/responses",
        post({
            let captured = captured.clone();
            move |headers: HeaderMap| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some(headers);
                    let mut response_headers = HeaderMap::new();
                    response_headers.insert(
                        "content-type",
                        HeaderValue::from_static("text/plain; charset=utf-8"),
                    );
                    response_headers.insert("cache-control", HeaderValue::from_static("no-cache"));
                    response_headers.insert(
                        "x-codex-turn-state",
                        HeaderValue::from_static("turn-state-123"),
                    );
                    response_headers
                        .insert("traceparent", HeaderValue::from_static("00-abc-123-01"));
                    let body = Body::from_stream(stream::iter(vec![Ok::<_, std::io::Error>(
                        Bytes::from_static(
                            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_stream\",\"object\":\"response\",\"created_at\":1,\"status\":\"completed\",\"background\":false}}\n\n",
                        ),
                    )]));
                    (response_headers, body)
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock streaming upstream");
    });
    (format!("http://{addr}"), handle)
}

async fn spawn_mock_compact_upstream(
    captured: Arc<Mutex<Option<(HeaderMap, Value)>>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/responses/compact",
        post({
            let captured = captured.clone();
            move |headers: HeaderMap, AxumJson(body): AxumJson<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().await = Some((headers, body));
                    let mut response_headers = HeaderMap::new();
                    response_headers.insert("cache-control", HeaderValue::from_static("no-store"));
                    response_headers.insert(
                        "x-codex-turn-state",
                        HeaderValue::from_static("turn-state-123"),
                    );
                    response_headers
                        .insert("traceparent", HeaderValue::from_static("00-abc-123-01"));
                    (
                        response_headers,
                        AxumJson(json!({
                            "object": "response.compaction",
                            "output": [
                                { "type": "compaction_summary", "encrypted_content": "abc" }
                            ]
                        })),
                    )
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock compact upstream");
    });
    (format!("http://{addr}"), handle)
}

async fn spawn_mock_models_upstream(
    request_count: Arc<Mutex<usize>>,
) -> (String, tokio::task::JoinHandle<()>) {
    spawn_mock_models_upstream_with_content_type(
        request_count,
        HeaderValue::from_static("application/json"),
    )
    .await
}

async fn spawn_mock_models_upstream_invalid_then_bad_request(
    request_count: Arc<Mutex<usize>>,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/models",
        get({
            let request_count = request_count.clone();
            move || {
                let request_count = request_count.clone();
                async move {
                    let mut count = request_count.lock().await;
                    *count += 1;
                    let mut response_headers = HeaderMap::new();
                    response_headers
                        .insert("content-type", HeaderValue::from_static("application/json"));
                    response_headers.insert("cache-control", HeaderValue::from_static("no-store"));
                    if *count == 1 {
                        return (
                            StatusCode::OK,
                            response_headers,
                            json!({ "object": "list" }).to_string(),
                        );
                    }
                    (
                        StatusCode::BAD_REQUEST,
                        response_headers,
                        json!({ "error": { "message": "bad request" } }).to_string(),
                    )
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock models upstream");
    });
    (format!("http://{addr}"), handle)
}

async fn spawn_mock_models_upstream_with_content_type(
    request_count: Arc<Mutex<usize>>,
    content_type: HeaderValue,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/models",
        get({
            let request_count = request_count.clone();
            let content_type = content_type.clone();
            move || {
                let request_count = request_count.clone();
                let content_type = content_type.clone();
                async move {
                    let mut count = request_count.lock().await;
                    *count += 1;
                    let body = if *count == 1 {
                        json!({ "object": "list" })
                    } else {
                        json!({
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
                        })
                    };
                    let mut response_headers = HeaderMap::new();
                    response_headers.insert("content-type", content_type);
                    response_headers.insert("cache-control", HeaderValue::from_static("no-store"));
                    (response_headers, body.to_string())
                }
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock models upstream");
    });
    (format!("http://{addr}"), handle)
}

async fn spawn_mock_responses_websocket_upstream(
    turn_state: &'static str,
) -> (String, tokio::task::JoinHandle<()>) {
    let app = Router::new().route(
        "/responses",
        get(move |ws: WebSocketUpgrade| async move {
            let mut headers = HeaderMap::new();
            headers.insert("x-codex-turn-state", HeaderValue::from_static(turn_state));
            (
                headers,
                ws.on_upgrade(|socket| async move {
                    drop(socket);
                }),
            )
                .into_response()
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve mock websocket upstream");
    });
    (format!("http://{addr}"), handle)
}

async fn spawn_public_test_server(state: AppState) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app = public_routes().with_state(state);
    let handle = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve public test app");
    });
    (format!("http://{addr}"), handle)
}

#[tokio::test]
async fn responses_prepare_rewrites_non_codex_string_input_before_upstream() {
    let (state, _account_id) = seeded_state().await;

    let prepared = prepare_responses_request(
        &state,
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "input": "hi",
            "stream": true
        }))),
    )
    .await
    .expect("request prepared");

    assert_eq!(
        prepared.failure_context.response_shape,
        crate::models::ResponseShape::OpenAi
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/type")
            .and_then(Value::as_str),
        Some("message")
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/role")
            .and_then(Value::as_str),
        Some("user")
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/content/0/type")
            .and_then(Value::as_str),
        Some("input_text")
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/content/0/text")
            .and_then(Value::as_str),
        Some("hi")
    );
}

#[tokio::test]
async fn compact_prepare_rewrites_non_codex_string_input_before_upstream() {
    let (state, _account_id) = seeded_state().await;

    let prepared = prepare_compact_request(
        &state,
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "input": "hi"
        }))),
    )
    .await
    .expect("request prepared");

    assert_eq!(
        prepared.failure_context.response_shape,
        crate::models::ResponseShape::OpenAi
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/type")
            .and_then(Value::as_str),
        Some("message")
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/role")
            .and_then(Value::as_str),
        Some("user")
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/content/0/type")
            .and_then(Value::as_str),
        Some("input_text")
    );
    assert_eq!(
        prepared
            .body
            .pointer("/input/0/content/0/text")
            .and_then(Value::as_str),
        Some("hi")
    );
}

#[tokio::test]
async fn compact_non_codex_invalid_gateway_returns_openai_json_error() {
    let (state, _account_id) = seeded_state().await;

    let response = post_responses_compact(
        State(state),
        OriginalUri(Uri::from_static("/v1/responses/compact")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "_gateway": 7
        }))),
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert!(json_body.get("error").is_some());
    assert!(json_body.pointer("/detail").is_none());
}

#[tokio::test]
async fn compact_non_codex_invalid_json_returns_openai_json_error() {
    let (state, _account_id) = seeded_state().await;

    let response = public_routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("originator", "openai-python")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .expect("request"),
        )
        .await
        .expect("router response");

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert!(json_body.get("error").is_some());
    assert!(json_body.pointer("/detail").is_none());
}

#[tokio::test]
async fn compact_non_codex_pool_block_returns_openai_json_error() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");

    let response = post_responses_compact(
        State(state),
        OriginalUri(Uri::from_static("/v1/responses/compact")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "input": "hi"
        }))),
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/error/code").and_then(Value::as_str),
        Some("server_is_overloaded")
    );
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("server_error")
    );
    assert!(json_body.pointer("/detail").is_none());
}

#[tokio::test]
async fn models_non_codex_pool_block_returns_openai_json_error() {
    let temp = tempdir().expect("tempdir");
    let state = AppState::new(test_config(temp.path().to_path_buf())).expect("state builds");

    let response = get_models(State(state), OriginalUri(Uri::from_static("/v1/models")), {
        let mut headers = HeaderMap::new();
        headers.insert("originator", HeaderValue::from_static("openai-python"));
        headers
    })
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/error/code").and_then(Value::as_str),
        Some("server_is_overloaded")
    );
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("server_error")
    );
}

#[tokio::test]
async fn models_non_codex_unreachable_upstream_returns_openai_json_error() {
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = "http://127.0.0.1:1".to_string();
    config.request_timeout_seconds = 1;
    let seeded = seeded_state_with_config(config).await;

    let response = get_models(
        State(seeded.state),
        OriginalUri(Uri::from_static("/v1/models")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("server_error")
    );
    assert_eq!(
        json_body.pointer("/error/code").and_then(Value::as_str),
        Some("server_is_overloaded")
    );
    assert!(json_body.pointer("/detail").is_none());
}

#[tokio::test]
async fn models_non_codex_success_preserves_openai_success_headers() {
    let request_count = Arc::new(Mutex::new(1usize));
    let (upstream_base_url, upstream_handle) = spawn_mock_models_upstream(request_count).await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_with_config(config).await;

    let response = get_models(
        State(seeded.state),
        OriginalUri(Uri::from_static("/v1/models")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/data/0/id").and_then(Value::as_str),
        Some("gpt-5.4")
    );

    upstream_handle.abort();
}

#[tokio::test]
async fn models_fail_over_when_first_account_returns_invalid_schema_json() {
    let request_count = Arc::new(Mutex::new(0usize));
    let (upstream_base_url, upstream_handle) =
        spawn_mock_models_upstream(request_count.clone()).await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_pair_with_config(config).await;

    let response = get_models(
        State(seeded.state.clone()),
        OriginalUri(Uri::from_static("/v1/models")),
        HeaderMap::new(),
    )
    .await;

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/data/0/id").and_then(Value::as_str),
        Some("gpt-5.4")
    );
    assert_eq!(*request_count.lock().await, 2);

    let accounts = seeded.state.accounts.write().await;
    let first = accounts
        .view(&seeded.account_ids[0])
        .expect("first account exists");
    let second = accounts
        .view(&seeded.account_ids[1])
        .expect("second account exists");
    let failed_accounts = [first, second]
        .into_iter()
        .filter(|view| view.last_error.is_some())
        .collect::<Vec<_>>();
    assert_eq!(failed_accounts.len(), 1);
    assert_eq!(
        failed_accounts[0].routing_state,
        crate::accounts::RoutingState::TemporarilyUnavailable
    );
    assert_eq!(
        failed_accounts[0].blocked_reason,
        Some(crate::accounts::BlockedReason::TemporarilyUnavailable)
    );

    upstream_handle.abort();
}

#[tokio::test]
async fn models_preserve_later_non_parse_failure() {
    let request_count = Arc::new(Mutex::new(0usize));
    let (upstream_base_url, _upstream_handle) =
        spawn_mock_models_upstream_invalid_then_bad_request(request_count.clone()).await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_pair_with_config(config).await;

    let response = get_models(
        State(seeded.state),
        OriginalUri(Uri::from_static("/v1/models")),
        HeaderMap::new(),
    )
    .await;

    let (status, _headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(*request_count.lock().await, 2);
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/error/message").and_then(Value::as_str),
        Some("bad request")
    );
}

#[tokio::test]
async fn responses_codex_invalid_gateway_returns_synthetic_sse() {
    let (state, _account_id) = seeded_state().await;

    let response = post_responses(
        State(state),
        OriginalUri(Uri::from_static("/v1/responses")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("codex-tui"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "_gateway": 7,
            "stream": true
        }))),
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
    assert!(text.contains("_gateway must be null or object"));
}

#[tokio::test]
async fn responses_non_codex_invalid_gateway_returns_openai_json_error() {
    let (state, _account_id) = seeded_state().await;

    let response = post_responses(
        State(state),
        OriginalUri(Uri::from_static("/v1/responses")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "_gateway": 7,
            "stream": true
        }))),
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("invalid_request_error")
    );
    assert!(json_body.pointer("/detail").is_none());
}

#[tokio::test]
async fn responses_prepare_stream_false_uses_unary_failure_mode() {
    let (state, _account_id) = seeded_state().await;

    let prepared = prepare_responses_request(
        &state,
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "input": "hi",
            "stream": false
        }))),
    )
    .await
    .expect("request prepared");

    assert_eq!(
        prepared.failure_context.render_mode,
        crate::gateway_errors::FailureRenderMode::UnaryJson
    );
}

#[tokio::test]
async fn responses_non_codex_stream_false_invalid_gateway_returns_unary_json_error() {
    let (state, _account_id) = seeded_state().await;

    let response = post_responses(
        State(state),
        OriginalUri(Uri::from_static("/v1/responses")),
        {
            let mut headers = HeaderMap::new();
            headers.insert("originator", HeaderValue::from_static("openai-python"));
            headers
        },
        Ok(Json(json!({
            "model": "gpt-5.4",
            "_gateway": 7,
            "stream": false
        }))),
    )
    .await;

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(!text.contains("response.failed"));
    let json_body: Value = serde_json::from_slice(text.as_bytes()).expect("json body");
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("invalid_request_error")
    );
}

#[tokio::test]
async fn responses_non_codex_stream_false_success_uses_unary_json_and_preserves_request_shape() {
    let captured = Arc::new(Mutex::new(None));
    let (upstream_base_url, upstream_handle) =
        spawn_mock_responses_upstream(captured.clone()).await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_with_config(config).await;

    let response = public_routes()
        .with_state(seeded.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("originator", "openai-python")
                .header("session_id", "sess_123")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.4",
                        "input": "hi",
                        "stream": false
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("router response");

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert!(headers.get("x-codex-turn-state").is_none());
    assert!(headers.get("traceparent").is_none());
    assert_eq!(
        serde_json::from_slice::<Value>(&body)
            .expect("json")
            .pointer("/id")
            .and_then(Value::as_str),
        Some("resp_test")
    );

    let (captured_headers, captured_body) = captured
        .lock()
        .await
        .clone()
        .expect("captured upstream request");
    assert_eq!(
        captured_headers
            .get("session_id")
            .and_then(|value| value.to_str().ok()),
        Some("sess_123")
    );
    assert_eq!(
        captured_body.get("stream").and_then(Value::as_bool),
        Some(false)
    );
    assert_eq!(
        captured_body
            .pointer("/client_metadata/x-codex-installation-id")
            .and_then(Value::as_str),
        Some(stable_installation_id("acct_123").as_str())
    );
    assert_eq!(
        captured_body
            .pointer("/input/0/content/0/text")
            .and_then(Value::as_str),
        Some("hi")
    );

    upstream_handle.abort();
}

#[tokio::test]
async fn responses_non_codex_streaming_success_filters_response_headers() {
    let captured = Arc::new(Mutex::new(None));
    let (upstream_base_url, upstream_handle) =
        spawn_mock_responses_stream_upstream(captured.clone()).await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_with_config(config).await;

    let response = public_routes()
        .with_state(seeded.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("originator", "openai-python")
                .header("session_id", "sess_123")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.4",
                        "input": "hi",
                        "stream": true
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("router response");

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-cache")
    );
    assert!(headers.get("x-codex-turn-state").is_none());
    assert!(headers.get("traceparent").is_none());
    let text = String::from_utf8(body.to_vec()).expect("utf8");
    assert!(text.contains("event: response.completed"));
    assert!(text.contains("\"id\":\"resp_stream\""));

    let captured_headers = captured
        .lock()
        .await
        .clone()
        .expect("captured upstream request");
    assert_eq!(
        captured_headers
            .get("session_id")
            .and_then(|value| value.to_str().ok()),
        Some("sess_123")
    );

    upstream_handle.abort();
}

#[tokio::test]
async fn compact_non_codex_success_filters_response_headers_and_rewrites_type() {
    let captured = Arc::new(Mutex::new(None));
    let (upstream_base_url, upstream_handle) = spawn_mock_compact_upstream(captured.clone()).await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_with_config(config).await;

    let response = public_routes()
        .with_state(seeded.state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses/compact")
                .header("originator", "openai-python")
                .header("session_id", "sess_123")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "model": "gpt-5.4",
                        "input": "hi"
                    })
                    .to_string(),
                ))
                .expect("request"),
        )
        .await
        .expect("router response");

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    assert_eq!(
        headers
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert!(headers.get("x-codex-turn-state").is_none());
    assert!(headers.get("traceparent").is_none());
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/output/0/type").and_then(Value::as_str),
        Some("compaction")
    );

    let (captured_headers, captured_body) = captured
        .lock()
        .await
        .clone()
        .expect("captured upstream request");
    assert_eq!(
        captured_headers
            .get("session_id")
            .and_then(|value| value.to_str().ok()),
        Some("sess_123")
    );
    assert_eq!(
        captured_body
            .pointer("/input/0/content/0/text")
            .and_then(Value::as_str),
        Some("hi")
    );

    upstream_handle.abort();
}

#[tokio::test]
async fn responses_codex_invalid_json_returns_synthetic_sse() {
    let (state, _account_id) = seeded_state().await;

    let response = public_routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("originator", "codex-tui")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .expect("request"),
        )
        .await
        .expect("router response");

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
    assert!(!text.contains("\"detail\""));
}

#[tokio::test]
async fn responses_non_codex_invalid_json_returns_openai_json_error() {
    let (state, _account_id) = seeded_state().await;

    let response = public_routes()
        .with_state(state)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("originator", "openai-python")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .expect("request"),
        )
        .await
        .expect("router response");

    let (status, headers, body) = response_parts(response).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        headers
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = serde_json::from_slice(&body).expect("json body");
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("invalid_request_error")
    );
    assert!(json_body.pointer("/detail").is_none());
}

#[tokio::test]
async fn responses_websocket_pre_upgrade_failure_uses_openai_error_shape() {
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = "http://127.0.0.1:1".to_string();
    config.request_timeout_seconds = 1;
    let seeded = seeded_state_with_config(config).await;
    let (server_base_url, server_handle) = spawn_public_test_server(seeded.state).await;

    let response = reqwest::Client::new()
        .get(format!("{server_base_url}/v1/responses"))
        .header("originator", "openai-python")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .send()
        .await
        .expect("request succeeds");

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok()),
        Some("application/json")
    );
    let json_body: Value = response.json().await.expect("json body");
    assert_eq!(
        json_body.pointer("/error/type").and_then(Value::as_str),
        Some("server_error")
    );
    assert_eq!(
        json_body.pointer("/error/code").and_then(Value::as_str),
        Some("server_is_overloaded")
    );

    server_handle.abort();
}

#[tokio::test]
async fn responses_websocket_success_propagates_turn_state_header() {
    let (upstream_base_url, upstream_handle) =
        spawn_mock_responses_websocket_upstream("turn-state-123").await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_with_config(config).await;
    let (server_base_url, server_handle) = spawn_public_test_server(seeded.state).await;

    let mut request = format!("{server_base_url}/v1/responses")
        .replace("http://", "ws://")
        .into_client_request()
        .expect("websocket request");
    request
        .headers_mut()
        .insert("originator", HeaderValue::from_static("codex-tui"));
    let (stream, response) = connect_async(request).await.expect("websocket connects");

    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert_eq!(
        response
            .headers()
            .get("x-codex-turn-state")
            .and_then(|value| value.to_str().ok()),
        Some("turn-state-123")
    );

    drop(stream);
    server_handle.abort();
    upstream_handle.abort();
}

#[tokio::test]
async fn responses_openai_websocket_success_does_not_propagate_turn_state_header() {
    let (upstream_base_url, upstream_handle) =
        spawn_mock_responses_websocket_upstream("turn-state-123").await;
    let mut config = test_config(std::path::PathBuf::from("/tmp/unused"));
    config.upstream_base_url = upstream_base_url;
    let seeded = seeded_state_with_config(config).await;
    let (server_base_url, server_handle) = spawn_public_test_server(seeded.state).await;

    let mut request = format!("{server_base_url}/v1/responses")
        .replace("http://", "ws://")
        .into_client_request()
        .expect("websocket request");
    request
        .headers_mut()
        .insert("originator", HeaderValue::from_static("openai-python"));
    let (stream, response) = connect_async(request).await.expect("websocket connects");

    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
    assert!(response.headers().get("x-codex-turn-state").is_none());

    drop(stream);
    server_handle.abort();
    upstream_handle.abort();
}
