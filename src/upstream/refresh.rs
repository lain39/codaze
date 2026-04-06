use super::{RefreshFailure, UpstreamClient};
use crate::accounts::RefreshedAccount;
use crate::classifier::FailureClass;
use crate::error_semantics::analyze_refresh_http;
use anyhow::Context;
use codex_login::CLIENT_ID;
use codex_login::token_data::{parse_chatgpt_jwt_claims, parse_jwt_expiration};
use http::StatusCode;
use http::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};

const REFRESH_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";

#[derive(Serialize)]
struct RefreshRequest {
    client_id: &'static str,
    grant_type: &'static str,
    refresh_token: String,
}

#[derive(Deserialize)]
struct RefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
}

impl UpstreamClient {
    pub async fn refresh_access_token(
        &self,
        refresh_token: String,
    ) -> Result<RefreshedAccount, RefreshFailure> {
        let body = RefreshRequest {
            client_id: CLIENT_ID,
            grant_type: "refresh_token",
            refresh_token,
        };

        let response = self
            .refresh_client
            .post(REFRESH_TOKEN_URL)
            .header(CONTENT_TYPE, "application/json")
            .json(&body);
        let response = if let Some(timeout) = self.request_timeout {
            response.timeout(timeout)
        } else {
            response
        }
        .send()
        .await
        .map_err(|error| RefreshFailure {
            status: StatusCode::BAD_GATEWAY,
            body: error.to_string(),
            class: FailureClass::TemporaryFailure,
            retry_after: None,
        })?;

        let status = response.status();
        if !status.is_success() {
            let response_headers = response.headers().clone();
            let body = response.text().await.unwrap_or_default();
            let semantics = analyze_refresh_http(status, Some(&response_headers), Some(&body));
            return Err(RefreshFailure {
                status,
                class: semantics.failure,
                body,
                retry_after: semantics.retry_after,
            });
        }

        let refreshed =
            response
                .json::<RefreshResponse>()
                .await
                .map_err(|error| RefreshFailure {
                    status: StatusCode::BAD_GATEWAY,
                    body: error.to_string(),
                    class: FailureClass::TemporaryFailure,
                    retry_after: None,
                })?;

        let access_token = refreshed
            .access_token
            .context("refresh response did not include access_token")
            .map_err(|error| RefreshFailure {
                status: StatusCode::BAD_GATEWAY,
                body: error.to_string(),
                class: FailureClass::TemporaryFailure,
                retry_after: None,
            })?;

        let claims = parse_chatgpt_jwt_claims(&access_token).map_err(|error| RefreshFailure {
            status: StatusCode::BAD_GATEWAY,
            body: error.to_string(),
            class: FailureClass::TemporaryFailure,
            retry_after: None,
        })?;
        let account_id = claims.chatgpt_account_id.clone();
        let plan_type = claims.get_chatgpt_plan_type_raw();
        let email = claims.email.clone();
        let expires_at = parse_jwt_expiration(&access_token).map_err(|error| RefreshFailure {
            status: StatusCode::BAD_GATEWAY,
            body: error.to_string(),
            class: FailureClass::TemporaryFailure,
            retry_after: None,
        })?;

        Ok(RefreshedAccount {
            access_token,
            refresh_token: refreshed.refresh_token,
            account_id,
            plan_type,
            email,
            access_token_expires_at: expires_at,
        })
    }
}
