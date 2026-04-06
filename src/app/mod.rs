mod admin;
mod api;
#[cfg(test)]
mod tests;

use crate::accounts::{
    AccountStore, ImportAccountResult, RefreshSuccessResult, RefreshedAccount,
    execute_account_disk_op, rescan_accounts_from_disk, startup_sync_accounts_from_disk,
};
use crate::classifier::FailureClass;
use crate::config::{RoutingPolicy, RuntimeConfig};
use crate::models::ModelsCache;
use crate::upstream::UpstreamClient;
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::{Mutex, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::warn;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<RuntimeConfig>,
    pub routing_policy: Arc<RwLock<RoutingPolicy>>,
    pub accounts: Arc<RwLock<AccountStore>>,
    pub account_disk_lock: Arc<Mutex<()>>,
    pub models_cache: Arc<RwLock<ModelsCache>>,
    pub models_refresh_in_flight: Arc<AtomicBool>,
    pub shutdown_token: CancellationToken,
    pub upstream: Arc<UpstreamClient>,
}

impl AppState {
    pub fn new(config: RuntimeConfig) -> anyhow::Result<Self> {
        let upstream = UpstreamClient::new(
            config.upstream_base_url.clone(),
            config.codex_version.clone(),
            config.fingerprint_mode,
            config.request_timeout_seconds,
        )?;
        let accounts = AccountStore::new(config.accounts_dir.clone());
        Ok(Self {
            routing_policy: Arc::new(RwLock::new(config.routing_policy)),
            config: Arc::new(config),
            accounts: Arc::new(RwLock::new(accounts)),
            account_disk_lock: Arc::new(Mutex::new(())),
            models_cache: Arc::new(RwLock::new(ModelsCache::default())),
            models_refresh_in_flight: Arc::new(AtomicBool::new(false)),
            shutdown_token: CancellationToken::new(),
            upstream: Arc::new(upstream),
        })
    }

    pub async fn sync_accounts_from_disk_startup(&self) -> anyhow::Result<()> {
        let _disk_guard = self.account_disk_lock.lock().await;
        startup_sync_accounts_from_disk(&self.config.accounts_dir, &self.accounts).await
    }

    pub async fn sync_accounts_from_disk_rescan(&self) -> anyhow::Result<()> {
        let _disk_guard = self.account_disk_lock.lock().await;
        rescan_accounts_from_disk(&self.config.accounts_dir, &self.accounts).await
    }

    pub async fn import_account(
        &self,
        refresh_token: String,
        label: Option<String>,
        email: Option<String>,
    ) -> anyhow::Result<ImportAccountResult> {
        let _disk_guard = self.account_disk_lock.lock().await;
        let plan = {
            self.accounts
                .read()
                .await
                .prepare_import_account(refresh_token, label, email)?
        };
        if let Some(disk_op) = &plan.disk_op {
            run_account_disk_op(disk_op.clone()).await?;
        }
        self.accounts.write().await.apply_import_account_plan(plan)
    }

    pub async fn remove_account(&self, id: &str) -> anyhow::Result<bool> {
        let _disk_guard = self.account_disk_lock.lock().await;
        let Some(plan) = self.accounts.read().await.prepare_remove_account(id)? else {
            return Ok(false);
        };
        run_account_disk_op(plan.disk_op.clone()).await?;
        self.accounts.write().await.apply_remove_account_plan(plan);
        Ok(true)
    }

    pub async fn finish_refresh_success(
        &self,
        account_id: &str,
        refreshed: RefreshedAccount,
    ) -> anyhow::Result<RefreshSuccessResult> {
        let _disk_guard = self.account_disk_lock.lock().await;
        let (mut result, persist_op) = self
            .accounts
            .write()
            .await
            .finish_refresh_success_without_persist(account_id, refreshed)?;
        if let Some(persist_op) = persist_op
            && let Err(error) = run_account_disk_op(persist_op).await
        {
            result.persist_warning = Some(format!("persist account file failed: {error}"));
        }
        Ok(result)
    }

    pub async fn finish_refresh_failure(
        &self,
        account_id: &str,
        failure: FailureClass,
        retry_after: Option<std::time::Duration>,
        details: String,
    ) -> anyhow::Result<()> {
        if failure == FailureClass::AuthInvalid {
            let _disk_guard = self.account_disk_lock.lock().await;
            let plan = self
                .accounts
                .write()
                .await
                .prepare_auth_invalid_failure(account_id, details)?;
            if let Some(trash_op) = &plan.trash_op
                && let Err(error) = run_account_disk_op(trash_op.clone()).await
            {
                warn!(
                    account_id = %plan.account_id,
                    %error,
                    "auth invalid detected but account file could not be moved to trash"
                );
                self.accounts
                    .write()
                    .await
                    .note_auth_invalid_trash_failure(&plan.account_id, &error.to_string());
                return Ok(());
            }
            self.accounts
                .write()
                .await
                .finalize_auth_invalid_failure(&plan.account_id)
        } else {
            self.accounts.write().await.finish_refresh_failure(
                account_id,
                failure,
                retry_after,
                details,
            )
        }
    }
}

async fn run_account_disk_op(op: crate::accounts::AccountDiskOp) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || execute_account_disk_op(&op))
        .await
        .map_err(anyhow::Error::from)??;
    Ok(())
}

pub fn public_routes() -> Router<AppState> {
    let api = Router::new()
        .route("/models", get(api::get_models))
        .route(
            "/responses",
            get(api::get_responses_websocket).post(api::post_responses),
        )
        .route("/responses/compact", post(api::post_responses_compact))
        .route(
            "/memories/trace_summarize",
            post(api::post_memories_trace_summarize),
        );

    Router::new()
        .route("/health", get(get_health))
        .nest("/v1", api)
}

pub fn admin_routes() -> Router<AppState> {
    Router::new()
        .route(
            "/admin/accounts/import",
            post(admin::post_admin_accounts_import),
        )
        .route(
            "/admin/accounts/wake",
            post(admin::post_admin_accounts_wake),
        )
        .route("/admin/accounts", get(admin::get_admin_accounts))
        .route(
            "/admin/accounts/{id}/wake",
            post(admin::post_admin_account_wake),
        )
        .route("/admin/accounts/{id}", delete(admin::delete_admin_account))
        .route(
            "/admin/routing/policy",
            get(admin::get_admin_routing_policy).put(admin::put_admin_routing_policy),
        )
}

async fn get_health() -> impl IntoResponse {
    Json(json!({ "ok": true }))
}

#[derive(Debug, Deserialize)]
struct ImportAccountRequest {
    refresh_token: String,
    label: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SetRoutingPolicyRequest {
    routing_policy: Option<String>,
}
