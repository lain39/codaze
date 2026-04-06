use super::client::retry_policy;
use super::headers::{
    add_auth_headers_to_header_map, build_models_extra_headers, build_responses_extra_headers,
    build_unary_extra_headers, sanitize_response_headers,
};
use super::{GatewayAuth, Provider, UpstreamClient, UpstreamStreamResponse, UpstreamUnaryResponse};
use crate::accounts::UpstreamAccount;
use anyhow::Context;
use bytes::Bytes;
use codex_client::{
    HttpTransport, Request, RequestCompression, Response, StreamResponse, TransportError,
    run_with_retry,
};
use http::header::ACCEPT;
use http::{HeaderMap, HeaderValue, Method};
use serde_json::Value;
use std::time::Duration;

impl UpstreamClient {
    pub async fn post_json(
        &self,
        path: &str,
        account: &UpstreamAccount,
        incoming_headers: &HeaderMap,
        body: Value,
    ) -> Result<UpstreamUnaryResponse, TransportError> {
        let extra_headers =
            build_unary_extra_headers(path, incoming_headers, self.fingerprint_mode);
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
        body: Value,
    ) -> Result<UpstreamStreamResponse, TransportError> {
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
        run_with_retry(
            retry_policy(&self.provider.retry),
            make_request,
            |req, _| self.transport.execute(req),
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
        run_with_retry(
            retry_policy(&self.provider.retry),
            make_request,
            |req, _| self.transport.stream(req),
        )
        .await
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
        req.body = Some(body.clone());
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

pub fn body_as_json(body: &Bytes) -> anyhow::Result<Value> {
    serde_json::from_slice(body).context("decode upstream json body")
}
