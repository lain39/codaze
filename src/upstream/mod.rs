mod client;
mod headers;
mod http;
mod refresh;
#[cfg(test)]
mod tests;
mod websocket;

use crate::accounts::UpstreamAccount;
use crate::classifier::FailureClass;
use crate::config::FingerprintMode;
use ::http::{HeaderMap, Method, StatusCode};
use bytes::Bytes;
use codex_client::{Request, RequestCompression, ReqwestTransport, TransportError};
use futures::stream::BoxStream;
use reqwest::Client;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub(crate) use self::headers::sanitize_response_headers;
pub use self::http::body_as_json;

const DEFAULT_REQUEST_MAX_RETRIES: u64 = 4;
const DEFAULT_REQUEST_RETRY_DELAY_MS: u64 = 200;

#[derive(Debug, Clone)]
struct Provider {
    base_url: String,
    headers: HeaderMap,
    retry: RetryConfig,
}

impl Provider {
    fn build_request(&self, method: Method, path: &str) -> Request {
        let base = self.base_url.trim_end_matches('/');
        let path = path.trim_start_matches('/');
        let url = if path.is_empty() {
            base.to_string()
        } else {
            format!("{base}/{path}")
        };

        Request {
            method,
            url,
            headers: self.headers.clone(),
            body: None,
            compression: RequestCompression::None,
            timeout: None,
        }
    }
}

#[derive(Debug, Clone)]
struct RetryConfig {
    max_attempts: u64,
    base_delay: Duration,
    retry_429: bool,
    retry_5xx: bool,
    retry_transport: bool,
}

pub struct UpstreamClient {
    provider: Provider,
    transport: ReqwestTransport,
    refresh_client: Client,
    codex_version: String,
    fingerprint_mode: FingerprintMode,
    request_timeout: Option<Duration>,
}

pub struct UpstreamUnaryResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
}

pub struct UpstreamStreamResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub bytes: BoxStream<'static, Result<Bytes, TransportError>>,
}

pub struct UpstreamWebsocketConnection {
    pub stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

#[derive(Debug)]
pub struct RefreshFailure {
    pub status: StatusCode,
    pub body: String,
    pub class: FailureClass,
    pub retry_after: Option<Duration>,
}

#[derive(Clone)]
struct GatewayAuth {
    access_token: String,
    account_id: Option<String>,
}

impl GatewayAuth {
    fn new(account: &UpstreamAccount) -> Self {
        Self {
            access_token: account.access_token.clone(),
            account_id: account.chatgpt_account_id.clone(),
        }
    }

    fn bearer_token(&self) -> Option<String> {
        Some(self.access_token.clone())
    }

    fn account_id(&self) -> Option<String> {
        self.account_id.clone()
    }
}
