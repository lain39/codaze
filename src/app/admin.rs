use super::{AppState, ImportAccountRequest, SetRoutingPolicyRequest};
use crate::accounts::{INVALID_REFRESH_TOKEN_MESSAGE, normalize_refresh_token};
use crate::config::RoutingPolicy;
use crate::gateway_errors::json_error;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

pub(crate) async fn post_admin_accounts_import(
    State(state): State<AppState>,
    Json(body): Json<ImportAccountRequest>,
) -> Response {
    let refresh_token = match normalize_refresh_token(&body.refresh_token) {
        Ok(refresh_token) => refresh_token,
        Err(_) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                INVALID_REFRESH_TOKEN_MESSAGE.to_string(),
            );
        }
    };
    match state
        .import_account(refresh_token, body.label, body.email)
        .await
    {
        Ok(result) => {
            let status = if result.already_exists {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            };
            (
                status,
                Json(json!({
                    "account": result.account,
                    "already_exists": result.already_exists,
                })),
            )
                .into_response()
        }
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

pub(crate) async fn get_admin_accounts(State(state): State<AppState>) -> Response {
    let mut accounts = state.accounts.write().await;
    let items = accounts.list();
    (StatusCode::OK, Json(json!({ "accounts": items }))).into_response()
}

pub(crate) async fn delete_admin_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.remove_account(&id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => json_error(StatusCode::NOT_FOUND, format!("unknown account `{id}`")),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

pub(crate) async fn post_admin_account_wake(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    let mut accounts = state.accounts.write().await;
    if accounts.view(&id).is_err() {
        return json_error(StatusCode::NOT_FOUND, format!("unknown account `{id}`"));
    }
    match accounts.wake_account(&id) {
        Ok(result) => (StatusCode::OK, Json(json!(result))).into_response(),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()),
    }
}

pub(crate) async fn post_admin_accounts_wake(State(state): State<AppState>) -> Response {
    let mut accounts = state.accounts.write().await;
    let result = accounts.wake_all_accounts();
    (StatusCode::OK, Json(json!(result))).into_response()
}

pub(crate) async fn get_admin_routing_policy(State(state): State<AppState>) -> Response {
    let policy = *state.routing_policy.read().await;
    (
        StatusCode::OK,
        Json(json!({
            "routing_policy": policy.as_str(),
        })),
    )
        .into_response()
}

pub(crate) async fn put_admin_routing_policy(
    State(state): State<AppState>,
    Json(body): Json<SetRoutingPolicyRequest>,
) -> Response {
    let Some(policy) = body.routing_policy else {
        return json_error(
            StatusCode::BAD_REQUEST,
            "missing routing_policy".to_string(),
        );
    };

    let policy = match RoutingPolicy::parse(&policy) {
        Ok(policy) => policy,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, error.to_string()),
    };

    *state.routing_policy.write().await = policy;

    (
        StatusCode::OK,
        Json(json!({
            "routing_policy": policy.as_str(),
        })),
    )
        .into_response()
}
