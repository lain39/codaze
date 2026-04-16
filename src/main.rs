mod accounts;
mod app;
mod classifier;
mod config;
mod error_semantics;
mod failover;
mod gateway_errors;
mod http_shape;
mod models;
mod request_normalization;
mod responses;
mod router;
mod upstream;

use anyhow::Context;
use app::AppState;
use codex_login::default_client::{SetOriginatorError, USER_AGENT_SUFFIX, set_default_originator};
use config::{RuntimeConfig, ensure_loopback_listener};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio::time::{Duration, MissedTickBehavior, timeout};
use tracing::info;
use tracing::warn;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("info,codex_client::custom_ca=warn")),
        )
        .with_target(false)
        .compact()
        .init();

    let config = RuntimeConfig::from_args()?;
    let public_bind_addr: SocketAddr = config.listen.parse()?;
    let admin_bind_addr: SocketAddr = config.admin_listen.parse()?;
    ensure_loopback_listener(public_bind_addr)?;
    ensure_loopback_listener(admin_bind_addr)?;
    initialize_codex_tui_fingerprint(&config.codex_version);
    let state = AppState::new(config)?;
    state.sync_accounts_from_disk_startup().await?;

    let public_router = app::public_routes().with_state(state.clone());
    let admin_router = app::admin_routes().with_state(state.clone());
    let public_listener = TcpListener::bind(public_bind_addr).await?;
    let admin_listener = TcpListener::bind(admin_bind_addr).await?;
    let scanner_task = spawn_accounts_scanner(state.clone());
    let public_shutdown = state.shutdown_token.clone();
    let admin_shutdown = state.shutdown_token.clone();
    let public_task = tokio::spawn(async move {
        axum::serve(public_listener, public_router)
            .with_graceful_shutdown(public_shutdown.cancelled_owned())
            .await
            .context("public server exited with error")
    });
    let admin_task = tokio::spawn(async move {
        axum::serve(admin_listener, admin_router)
            .with_graceful_shutdown(admin_shutdown.cancelled_owned())
            .await
            .context("admin server exited with error")
    });
    let routing_policy = *state.routing_policy.read().await;
    let shutdown_signal = wait_for_shutdown_signal();
    info!(
        public_listen = %public_bind_addr,
        admin_listen = %admin_bind_addr,
        routing_policy = routing_policy.as_str(),
        fingerprint_mode = state.config.fingerprint_mode.as_str(),
        "Codaze☆ listening"
    );
    tokio::pin!(scanner_task);
    tokio::pin!(public_task);
    tokio::pin!(admin_task);
    tokio::pin!(shutdown_signal);

    tokio::select! {
        _ = &mut shutdown_signal => {
            info!("shutdown signal received");
            state.shutdown_token.cancel();
            wait_for_graceful_shutdown(
                &state,
                &mut public_task,
                &mut admin_task,
                &mut scanner_task,
            ).await
        }
        result = &mut public_task => {
            state.shutdown_token.cancel();
            let _ = scanner_task.await;
            let _ = admin_task.await;
            flatten_service_task("public", result)
        }
        result = &mut admin_task => {
            state.shutdown_token.cancel();
            let _ = scanner_task.await;
            let _ = public_task.await;
            flatten_service_task("admin", result)
        }
        result = &mut scanner_task => {
            state.shutdown_token.cancel();
            let _ = public_task.await;
            let _ = admin_task.await;
            flatten_scanner_task(result)
        }
    }
}

fn spawn_accounts_scanner(state: AppState) -> JoinHandle<()> {
    let interval_seconds = state.config.accounts_scan_interval_seconds;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(interval_seconds));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(error) = state.sync_accounts_from_disk_rescan().await {
                        warn!(%error, "failed to rescan accounts directory");
                    }
                }
                _ = state.shutdown_token.cancelled() => {
                    break;
                }
            }
        }
    })
}

async fn wait_for_graceful_shutdown(
    state: &AppState,
    public_task: &mut JoinHandle<anyhow::Result<()>>,
    admin_task: &mut JoinHandle<anyhow::Result<()>>,
    scanner_task: &mut JoinHandle<()>,
) -> anyhow::Result<()> {
    let grace_period = Duration::from_secs(state.config.shutdown_grace_period_seconds);
    let shutdown_wait = async {
        flatten_service_task("public", (&mut *public_task).await)?;
        flatten_service_task("admin", (&mut *admin_task).await)?;
        flatten_scanner_task((&mut *scanner_task).await)?;
        Ok::<(), anyhow::Error>(())
    };

    match timeout(grace_period, shutdown_wait).await {
        Ok(result) => result,
        Err(_) => {
            warn!(
                grace_period_seconds = state.config.shutdown_grace_period_seconds,
                "graceful shutdown timed out; aborting remaining tasks"
            );
            public_task.abort();
            admin_task.abort();
            scanner_task.abort();
            let _ = public_task.await;
            let _ = admin_task.await;
            let _ = scanner_task.await;
            Ok(())
        }
    }
}

fn flatten_service_task(
    name: &str,
    result: Result<anyhow::Result<()>, tokio::task::JoinError>,
) -> anyhow::Result<()> {
    match result {
        Ok(inner) => inner,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error).context(format!("{name} task join failed")),
    }
}

fn flatten_scanner_task(result: Result<(), tokio::task::JoinError>) -> anyhow::Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(error).context("accounts scanner task join failed"),
    }
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};

        let ctrl_c = tokio::signal::ctrl_c();
        let mut terminate = signal(SignalKind::terminate()).expect("install SIGTERM handler");

        tokio::select! {
            _ = ctrl_c => {}
            _ = terminate.recv() => {}
        }
    }

    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

fn initialize_codex_tui_fingerprint(codex_version: &str) {
    if let Err(error) = set_default_originator("codex-tui".to_string()) {
        match error {
            SetOriginatorError::AlreadyInitialized => {}
            SetOriginatorError::InvalidHeaderValue => {
                warn!("failed to set codex-tui originator override");
            }
        }
    }

    if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
        *suffix = Some(format!("codex-tui; {codex_version}"));
    }
}
