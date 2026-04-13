use super::fingerprint::installation_id_for_account;
use super::headers::{add_auth_headers_to_header_map, build_responses_websocket_headers};
use super::{GatewayAuth, UpstreamClient, UpstreamWebsocketConnection};
use crate::accounts::UpstreamAccount;
use codex_client::TransportError;
use http::HeaderMap;
use tokio_tungstenite::connect_async_tls_with_config;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tungstenite::extensions::ExtensionsConfig;
use tungstenite::extensions::compression::deflate::DeflateConfig;
use tungstenite::protocol::WebSocketConfig;
use url::Url;

impl UpstreamClient {
    pub async fn connect_responses_websocket(
        &self,
        account: &UpstreamAccount,
        incoming_headers: &HeaderMap,
    ) -> Result<UpstreamWebsocketConnection, TransportError> {
        let ws_url = websocket_url_for_path(&self.provider.base_url, "responses")
            .map_err(|error| TransportError::Build(error.to_string()))?;
        let mut headers = build_responses_websocket_headers(
            incoming_headers,
            self.fingerprint_mode,
            &self.codex_version,
        );
        add_auth_headers_to_header_map(&GatewayAuth::new(account), &mut headers);

        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|error| TransportError::Build(error.to_string()))?;
        request.headers_mut().extend(headers);

        let connector = codex_client::maybe_build_rustls_client_config_with_custom_ca()
            .map_err(|error| TransportError::Build(error.to_string()))?
            .map(tokio_tungstenite::Connector::Rustls);

        let (stream, _response) =
            connect_async_tls_with_config(request, Some(websocket_config()), false, connector)
                .await
                .map_err(|error| map_ws_error(error, &ws_url))?;

        Ok(UpstreamWebsocketConnection {
            stream,
            installation_id: installation_id_for_account(account, self.fingerprint_mode),
        })
    }
}

fn websocket_url_for_path(base_url: &str, path: &str) -> Result<Url, url::ParseError> {
    let base = base_url.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let url = if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    };
    let mut url = Url::parse(&url)?;
    let scheme = match url.scheme() {
        "http" => "ws",
        "https" => "wss",
        "ws" | "wss" => return Ok(url),
        _ => return Ok(url),
    };
    let _ = url.set_scheme(scheme);
    Ok(url)
}

fn websocket_config() -> WebSocketConfig {
    let mut extensions = ExtensionsConfig::default();
    extensions.permessage_deflate = Some(DeflateConfig::default());

    let mut config = WebSocketConfig::default();
    config.extensions = extensions;
    config
}

fn map_ws_error(err: WsError, url: &Url) -> TransportError {
    match err {
        WsError::Http(response) => {
            let status = response.status();
            let headers = response.headers().clone();
            let body = response
                .body()
                .as_ref()
                .and_then(|bytes| String::from_utf8(bytes.clone()).ok());
            TransportError::Http {
                status,
                url: Some(url.to_string()),
                headers: Some(headers),
                body,
            }
        }
        WsError::ConnectionClosed | WsError::AlreadyClosed => {
            TransportError::Network("websocket closed".to_string())
        }
        WsError::Io(error) => TransportError::Network(error.to_string()),
        other => TransportError::Network(other.to_string()),
    }
}
