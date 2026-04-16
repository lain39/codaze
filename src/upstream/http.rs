use super::client::retry_policy;
use super::fingerprint::{apply_responses_installation_id, installation_id_for_account};
use super::headers::{
    add_auth_headers_to_header_map, build_models_extra_headers, build_responses_extra_headers,
    build_unary_extra_headers, sanitize_response_headers,
};
use super::{GatewayAuth, Provider, UpstreamClient, UpstreamStreamResponse, UpstreamUnaryResponse};
use crate::accounts::UpstreamAccount;
use crate::classifier::{FailureClass, classify_request_http};
use anyhow::Context;
use bytes::Bytes;
use codex_client::{
    HttpTransport, Request, RequestBody, RequestCompression, Response, RetryPolicy, StreamResponse,
    TransportError, backoff,
};
use http::header::ACCEPT;
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;
use std::time::Duration;

impl UpstreamClient {
    pub async fn post_responses_json(
        &self,
        account: &UpstreamAccount,
        incoming_headers: &HeaderMap,
        mut body: Value,
    ) -> Result<UpstreamUnaryResponse, TransportError> {
        apply_responses_installation_id(
            &mut body,
            installation_id_for_account(account, self.fingerprint_mode).as_deref(),
            self.fingerprint_mode,
        );
        let extra_headers = build_responses_extra_headers(incoming_headers, self.fingerprint_mode);
        let response = self
            .execute_with(
                Method::POST,
                "responses",
                account,
                extra_headers,
                Some(body),
                |_| {},
            )
            .await?;
        Ok(UpstreamUnaryResponse {
            status: response.status,
            headers: sanitize_response_headers(&response.headers),
            body: response.body,
        })
    }

    pub async fn post_json(
        &self,
        path: &str,
        account: &UpstreamAccount,
        incoming_headers: &HeaderMap,
        body: Value,
    ) -> Result<UpstreamUnaryResponse, TransportError> {
        let installation_id = installation_id_for_account(account, self.fingerprint_mode);
        let extra_headers = build_unary_extra_headers(
            path,
            incoming_headers,
            self.fingerprint_mode,
            installation_id.as_deref(),
        );
        let response = self
            .execute_with(
                Method::POST,
                path,
                account,
                extra_headers,
                Some(body),
                |_| {},
            )
            .await?;
        Ok(UpstreamUnaryResponse {
            status: response.status,
            headers: sanitize_response_headers(&response.headers),
            body: response.body,
        })
    }

    pub async fn get_models(
        &self,
        account: &UpstreamAccount,
        incoming_headers: &HeaderMap,
    ) -> Result<UpstreamUnaryResponse, TransportError> {
        let extra_headers = build_models_extra_headers(incoming_headers, self.fingerprint_mode);
        let response = self
            .execute_with(Method::GET, "models", account, extra_headers, None, |req| {
                append_client_version_query(req, &self.codex_version)
            })
            .await?;
        Ok(UpstreamUnaryResponse {
            status: response.status,
            headers: sanitize_response_headers(&response.headers),
            body: response.body,
        })
    }

    pub async fn stream_json(
        &self,
        path: &str,
        account: &UpstreamAccount,
        incoming_headers: &HeaderMap,
        mut body: Value,
    ) -> Result<UpstreamStreamResponse, TransportError> {
        apply_responses_installation_id(
            &mut body,
            installation_id_for_account(account, self.fingerprint_mode).as_deref(),
            self.fingerprint_mode,
        );
        let extra_headers = build_responses_extra_headers(incoming_headers, self.fingerprint_mode);
        let response = self
            .stream_with(
                Method::POST,
                path,
                account,
                extra_headers,
                Some(body),
                configure_responses_stream_request,
            )
            .await?;
        Ok(UpstreamStreamResponse {
            status: response.status,
            headers: sanitize_response_headers(&response.headers),
            bytes: response.bytes,
        })
    }

    async fn execute_with<C>(
        &self,
        method: Method,
        path: &str,
        account: &UpstreamAccount,
        extra_headers: HeaderMap,
        body: Option<Value>,
        configure: C,
    ) -> Result<Response, TransportError>
    where
        C: Fn(&mut Request),
    {
        let auth = GatewayAuth::new(account);
        let transport = self.http_transport();
        let make_request = || {
            let mut req = make_request(
                &self.provider,
                &auth,
                &method,
                path,
                &extra_headers,
                body.as_ref(),
            );
            apply_http_request_timeout(&mut req, self.unary_request_timeout);
            configure(&mut req);
            req
        };
        execute_with_semantic_retry(
            retry_policy(&self.provider.retry),
            make_request,
            |req, _| transport.execute(req),
        )
        .await
    }

    async fn stream_with<C>(
        &self,
        method: Method,
        path: &str,
        account: &UpstreamAccount,
        extra_headers: HeaderMap,
        body: Option<Value>,
        configure: C,
    ) -> Result<StreamResponse, TransportError>
    where
        C: Fn(&mut Request),
    {
        let auth = GatewayAuth::new(account);
        let transport = self.http_transport();
        let make_request = || {
            let mut req = make_request(
                &self.provider,
                &auth,
                &method,
                path,
                &extra_headers,
                body.as_ref(),
            );
            apply_http_request_timeout(&mut req, self.stream_request_timeout);
            configure(&mut req);
            req
        };
        execute_with_semantic_retry(
            retry_policy(&self.provider.retry),
            make_request,
            |req, _| async move {
                match self.stream_connect_timeout {
                    Some(timeout_duration) => {
                        tokio::time::timeout(timeout_duration, transport.stream(req))
                            .await
                            .map_err(|_| TransportError::Timeout)?
                    }
                    None => transport.stream(req).await,
                }
            },
        )
        .await
    }

    fn http_transport(&self) -> &codex_client::ReqwestTransport {
        if self.fingerprint_mode == crate::config::FingerprintMode::Passthrough {
            &self.passthrough_transport
        } else {
            &self.codex_transport
        }
    }
}

fn make_request(
    provider: &Provider,
    auth: &GatewayAuth,
    method: &Method,
    path: &str,
    extra_headers: &HeaderMap,
    body: Option<&Value>,
) -> Request {
    let mut req = provider.build_request(method.clone(), path);
    req.headers.extend(extra_headers.clone());
    if let Some(body) = body {
        req.body = Some(RequestBody::Json(body.clone()));
    }
    add_auth_headers(auth, req)
}

fn add_auth_headers(auth: &GatewayAuth, mut req: Request) -> Request {
    add_auth_headers_to_header_map(auth, &mut req.headers);
    req
}

pub(super) fn append_client_version_query(req: &mut Request, client_version: &str) {
    let separator = if req.url.contains('?') { '&' } else { '?' };
    req.url = format!("{}{}client_version={client_version}", req.url, separator);
}

pub(super) fn configure_responses_stream_request(req: &mut Request) {
    req.headers
        .insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
    req.compression = RequestCompression::Zstd;
}

pub(super) fn apply_http_request_timeout(req: &mut Request, timeout: Option<Duration>) {
    req.timeout = timeout;
}

pub(super) async fn execute_with_semantic_retry<T, F, Fut>(
    policy: RetryPolicy,
    mut make_request: impl FnMut() -> Request,
    op: F,
) -> Result<T, TransportError>
where
    F: Fn(Request, u64) -> Fut,
    Fut: std::future::Future<Output = Result<T, TransportError>>,
{
    let mut last_error = None;
    for attempt in 0..=policy.max_attempts {
        let req = make_request();
        match op(req, attempt).await {
            Ok(response) => return Ok(response),
            Err(error) if should_retry_request_error(&policy, &error, attempt) => {
                last_error = Some(error);
                tokio::time::sleep(backoff(policy.base_delay, attempt + 1)).await;
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or(TransportError::RetryLimit))
}

pub(super) fn should_retry_request_error(
    policy: &RetryPolicy,
    error: &TransportError,
    attempt: u64,
) -> bool {
    if attempt >= policy.max_attempts {
        return false;
    }

    match error {
        TransportError::Http { status, body, .. } => {
            (policy.retry_on.retry_429 && status.as_u16() == 429)
                || (policy.retry_on.retry_5xx
                    && status.is_server_error()
                    && should_retry_http_failure(*status, body.as_deref()))
        }
        TransportError::Timeout | TransportError::Network(_) => policy.retry_on.retry_transport,
        TransportError::Build(_) | TransportError::RetryLimit => false,
    }
}

fn should_retry_http_failure(status: http::StatusCode, body: Option<&str>) -> bool {
    matches!(
        classify_request_http(status, body),
        FailureClass::TemporaryFailure | FailureClass::InternalFailure
    )
}

pub fn body_as_json(body: &Bytes) -> anyhow::Result<Value> {
    serde_json::from_slice(body).context("decode upstream json body")
}
