use super::{
    DEFAULT_REQUEST_MAX_RETRIES, DEFAULT_REQUEST_RETRY_DELAY_MS, Provider, RetryConfig,
    UpstreamClient,
};
use crate::config::FingerprintMode;
use anyhow::Context;
use codex_client::{
    BuildCustomCaTransportError, RetryOn, RetryPolicy, build_reqwest_client_with_custom_ca,
};
use codex_login::default_client::{USER_AGENT_SUFFIX, default_headers, originator};
use codex_terminal_detection::user_agent;
use codex_utils_rustls_provider::ensure_rustls_crypto_provider;
use http::HeaderValue;
use os_info::Info;
use std::env;
use std::time::Duration;

impl UpstreamClient {
    pub fn new(
        base_url: String,
        codex_version: String,
        fingerprint_mode: FingerprintMode,
        stream_timeout_seconds: u64,
    ) -> anyhow::Result<Self> {
        let client = try_build_codex_reqwest_client(&codex_version)
            .context("build Codex-aligned reqwest client")?;
        Ok(Self {
            provider: Provider {
                base_url: base_url.trim_end_matches('/').to_string(),
                headers: http::HeaderMap::new(),
                retry: RetryConfig {
                    max_attempts: DEFAULT_REQUEST_MAX_RETRIES,
                    base_delay: Duration::from_millis(DEFAULT_REQUEST_RETRY_DELAY_MS),
                    retry_429: false,
                    retry_5xx: true,
                    retry_transport: true,
                },
            },
            transport: codex_client::ReqwestTransport::new(client.clone()),
            refresh_client: client,
            codex_version,
            fingerprint_mode,
            request_timeout: Some(Duration::from_secs(stream_timeout_seconds)),
        })
    }
}

pub(super) fn try_build_codex_reqwest_client(
    codex_version: &str,
) -> Result<reqwest::Client, BuildCustomCaTransportError> {
    ensure_rustls_crypto_provider();
    let ua = build_codex_user_agent(codex_version);
    let mut builder = reqwest::Client::builder()
        .user_agent(ua)
        .default_headers(default_headers());
    if is_sandboxed() {
        builder = builder.no_proxy();
    }
    build_reqwest_client_with_custom_ca(builder)
}

fn is_sandboxed() -> bool {
    env::var("CODEX_SANDBOX").as_deref() == Ok("seatbelt")
}

pub(super) fn build_codex_user_agent(codex_version: &str) -> String {
    let os = os_info::get();
    let originator = originator();
    let prefix = format!(
        "{}/{codex_version} ({} {}; {}) {}",
        originator.value.as_str(),
        format_os_type(&os),
        os.version(),
        os.architecture().unwrap_or("unknown"),
        user_agent()
    );
    let suffix = user_agent_suffix(codex_version);
    sanitize_user_agent(format!("{prefix}{suffix}"), &prefix)
}

fn user_agent_suffix(codex_version: &str) -> String {
    let configured = USER_AGENT_SUFFIX
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    configured
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || format!(" (codex-tui; {codex_version})"),
            |value| format!(" ({value})"),
        )
}

fn format_os_type(os: &Info) -> String {
    os.os_type().to_string()
}

fn sanitize_user_agent(candidate: String, fallback: &str) -> String {
    if HeaderValue::from_str(candidate.as_str()).is_ok() {
        return candidate;
    }

    let sanitized: String = candidate
        .chars()
        .map(|ch| if matches!(ch, ' '..='~') { ch } else { '_' })
        .collect();
    if !sanitized.is_empty() && HeaderValue::from_str(sanitized.as_str()).is_ok() {
        sanitized
    } else if HeaderValue::from_str(fallback).is_ok() {
        fallback.to_string()
    } else {
        originator().value
    }
}

pub(super) fn retry_policy(retry: &RetryConfig) -> RetryPolicy {
    RetryPolicy {
        max_attempts: retry.max_attempts,
        base_delay: retry.base_delay,
        retry_on: RetryOn {
            retry_429: retry.retry_429,
            retry_5xx: retry.retry_5xx,
            retry_transport: retry.retry_transport,
        },
    }
}
