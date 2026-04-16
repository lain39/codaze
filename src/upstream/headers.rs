use super::GatewayAuth;
use super::client::build_codex_user_agent;
use super::fingerprint::{
    X_CODEX_INSTALLATION_ID_HEADER, X_CODEX_TURN_METADATA_HEADER,
    apply_compact_installation_id_header, default_turn_metadata_value,
    merge_turn_metadata_thread_source,
};
use crate::config::FingerprintMode;
use codex_login::default_client::default_headers;
use codex_protocol::protocol::{SessionSource, SubAgentSource};
use http::header::{
    AUTHORIZATION, CONNECTION, CONTENT_ENCODING, CONTENT_LENGTH, PROXY_AUTHENTICATE,
    PROXY_AUTHORIZATION, TE, TRAILER, TRANSFER_ENCODING, UPGRADE,
};
use http::{HeaderMap, HeaderName, HeaderValue};

pub(super) const OPENAI_BETA_HEADER: &str = "OpenAI-Beta";
pub(super) const RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE: &str =
    "responses_websockets=2026-02-06";
pub(super) const SESSION_SOURCE_HEADER: &str = "x-codex-session-source";
const X_CODEX_PARENT_THREAD_ID_HEADER: &str = "x-codex-parent-thread-id";
const X_CODEX_WINDOW_ID_HEADER: &str = "x-codex-window-id";
const X_OPENAI_SUBAGENT_HEADER: &str = "x-openai-subagent";
const KEEP_ALIVE_HEADER: &str = "keep-alive";

pub(super) fn add_auth_headers_to_header_map(auth: &GatewayAuth, headers: &mut HeaderMap) {
    if let Some(token) = auth.bearer_token()
        && let Ok(header) = HeaderValue::from_str(&format!("Bearer {token}"))
    {
        let _ = headers.insert(AUTHORIZATION, header);
    }
    if let Some(account_id) = auth.account_id()
        && let Ok(header) = HeaderValue::from_str(&account_id)
    {
        let _ = headers.insert("ChatGPT-Account-ID", header);
    }
}

pub(super) fn build_responses_extra_headers(
    incoming: &HeaderMap,
    mode: FingerprintMode,
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    extend_fingerprint_headers(incoming, &mut headers, mode);
    extend_responses_context_headers(incoming, &mut headers);
    extend_responses_identity_headers(incoming, &mut headers, mode);
    maybe_normalize_turn_metadata_header(&mut headers, mode);
    if mode == FingerprintMode::Normalize && !headers.contains_key("x-client-request-id") {
        maybe_copy_session_id_to_client_request_id(incoming, &mut headers);
    }
    headers
}

pub(super) fn build_unary_extra_headers(
    path: &str,
    incoming: &HeaderMap,
    mode: FingerprintMode,
    installation_id: Option<&str>,
) -> HeaderMap {
    match path {
        "responses/compact" => {
            let mut headers = HeaderMap::new();
            extend_fingerprint_headers(incoming, &mut headers, mode);
            extend_responses_context_headers(incoming, &mut headers);
            extend_responses_identity_headers(incoming, &mut headers, mode);
            maybe_normalize_turn_metadata_header(&mut headers, mode);
            if mode == FingerprintMode::Passthrough {
                copy_if_present(incoming, &mut headers, X_CODEX_INSTALLATION_ID_HEADER);
            }
            apply_compact_installation_id_header(&mut headers, installation_id, mode);
            if mode == FingerprintMode::Normalize && !headers.contains_key("x-client-request-id") {
                maybe_copy_session_id_to_client_request_id(incoming, &mut headers);
            }
            headers
        }
        "memories/trace_summarize" => {
            let mut headers = HeaderMap::new();
            extend_fingerprint_headers(incoming, &mut headers, mode);
            copy_if_present(incoming, &mut headers, X_OPENAI_SUBAGENT_HEADER);
            maybe_insert_subagent_from_session_source(incoming, &mut headers, mode);
            headers
        }
        _ => HeaderMap::new(),
    }
}

pub(super) fn build_models_extra_headers(incoming: &HeaderMap, mode: FingerprintMode) -> HeaderMap {
    let mut headers = HeaderMap::new();
    extend_fingerprint_headers(incoming, &mut headers, mode);
    headers
}

pub(super) fn build_responses_websocket_headers(
    incoming: &HeaderMap,
    mode: FingerprintMode,
    codex_version: &str,
) -> HeaderMap {
    let mut headers = if mode == FingerprintMode::Passthrough {
        HeaderMap::new()
    } else {
        let mut headers = default_headers();
        insert_header(
            &mut headers,
            "user-agent",
            &build_codex_user_agent(codex_version),
        );
        headers
    };
    copy_if_present(incoming, &mut headers, "session_id");
    copy_if_present(incoming, &mut headers, "x-client-request-id");
    copy_if_present(incoming, &mut headers, "x-codex-beta-features");
    copy_if_present(incoming, &mut headers, "x-codex-turn-state");
    copy_if_present(incoming, &mut headers, X_CODEX_TURN_METADATA_HEADER);
    extend_responses_identity_headers(incoming, &mut headers, mode);
    maybe_normalize_turn_metadata_header(&mut headers, mode);
    copy_if_present(
        incoming,
        &mut headers,
        "x-responsesapi-include-timing-metrics",
    );
    copy_if_present(incoming, &mut headers, "traceparent");
    copy_if_present(incoming, &mut headers, "tracestate");
    if mode == FingerprintMode::Passthrough {
        copy_if_present(incoming, &mut headers, "user-agent");
        copy_if_present(incoming, &mut headers, "originator");
        copy_if_present(incoming, &mut headers, "x-openai-internal-codex-residency");
    }
    if mode == FingerprintMode::Normalize && !headers.contains_key("x-client-request-id") {
        maybe_copy_session_id_to_client_request_id(incoming, &mut headers);
    }
    headers.insert(
        OPENAI_BETA_HEADER,
        HeaderValue::from_static(RESPONSES_WEBSOCKETS_V2_BETA_HEADER_VALUE),
    );
    headers
}

fn extend_fingerprint_headers(source: &HeaderMap, dest: &mut HeaderMap, mode: FingerprintMode) {
    if mode == FingerprintMode::Passthrough {
        copy_if_present(source, dest, "user-agent");
        copy_if_present(source, dest, "originator");
        copy_if_present(source, dest, "x-openai-internal-codex-residency");
    }
}

fn maybe_copy_session_id_to_client_request_id(source: &HeaderMap, dest: &mut HeaderMap) {
    if let Some(value) = source.get("session_id")
        && let Ok(value) = value.to_str()
    {
        insert_header(dest, "x-client-request-id", value);
    }
}

fn extend_responses_identity_headers(
    source: &HeaderMap,
    dest: &mut HeaderMap,
    mode: FingerprintMode,
) {
    copy_if_present(source, dest, X_CODEX_WINDOW_ID_HEADER);
    copy_if_present(source, dest, X_CODEX_PARENT_THREAD_ID_HEADER);
    copy_if_present(source, dest, X_OPENAI_SUBAGENT_HEADER);
    maybe_insert_parent_thread_id_from_session_source(source, dest, mode);
    maybe_insert_subagent_from_session_source(source, dest, mode);
}

fn extend_responses_context_headers(source: &HeaderMap, dest: &mut HeaderMap) {
    copy_if_present(source, dest, "session_id");
    copy_if_present(source, dest, "x-client-request-id");
    copy_if_present(source, dest, "x-codex-beta-features");
    copy_if_present(source, dest, "x-codex-turn-state");
    copy_if_present(source, dest, X_CODEX_TURN_METADATA_HEADER);
    copy_if_present(source, dest, "x-responsesapi-include-timing-metrics");
    copy_if_present(source, dest, "traceparent");
    copy_if_present(source, dest, "tracestate");
}

fn maybe_normalize_turn_metadata_header(headers: &mut HeaderMap, mode: FingerprintMode) {
    if mode != FingerprintMode::Normalize {
        return;
    }

    let normalized = match headers
        .get(X_CODEX_TURN_METADATA_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some(raw) => merge_turn_metadata_thread_source(raw),
        None => Some(default_turn_metadata_value()),
    };

    let Some(normalized) = normalized else {
        return;
    };

    insert_header(headers, X_CODEX_TURN_METADATA_HEADER, &normalized);
}

fn maybe_insert_parent_thread_id_from_session_source(
    source: &HeaderMap,
    dest: &mut HeaderMap,
    mode: FingerprintMode,
) {
    if mode != FingerprintMode::Normalize || dest.contains_key(X_CODEX_PARENT_THREAD_ID_HEADER) {
        return;
    }

    let Some(session_source) = parse_session_source_header(source) else {
        return;
    };
    let Some(parent_thread_id) = parent_thread_id_header_value(&session_source) else {
        return;
    };
    insert_header(dest, X_CODEX_PARENT_THREAD_ID_HEADER, &parent_thread_id);
}

fn maybe_insert_subagent_from_session_source(
    source: &HeaderMap,
    dest: &mut HeaderMap,
    mode: FingerprintMode,
) {
    if mode != FingerprintMode::Normalize || dest.contains_key(X_OPENAI_SUBAGENT_HEADER) {
        return;
    }

    let Some(session_source) = parse_session_source_header(source) else {
        return;
    };
    let Some(subagent) = subagent_header_value(&session_source) else {
        return;
    };
    insert_header(dest, X_OPENAI_SUBAGENT_HEADER, &subagent);
}

fn parse_session_source_header(source: &HeaderMap) -> Option<SessionSource> {
    let raw = source.get(SESSION_SOURCE_HEADER)?.to_str().ok()?.trim();
    if raw.is_empty() {
        return None;
    }

    serde_json::from_str::<SessionSource>(raw).ok()
}

fn subagent_header_value(source: &SessionSource) -> Option<String> {
    let SessionSource::SubAgent(subagent) = source else {
        return None;
    };

    match subagent {
        SubAgentSource::Review => Some("review".to_string()),
        SubAgentSource::Compact => Some("compact".to_string()),
        SubAgentSource::MemoryConsolidation => Some("memory_consolidation".to_string()),
        SubAgentSource::ThreadSpawn { .. } => Some("collab_spawn".to_string()),
        SubAgentSource::Other(label) => Some(label.clone()),
    }
}

fn parent_thread_id_header_value(source: &SessionSource) -> Option<String> {
    let SessionSource::SubAgent(SubAgentSource::ThreadSpawn {
        parent_thread_id, ..
    }) = source
    else {
        return None;
    };

    Some(parent_thread_id.to_string())
}

fn insert_header(headers: &mut HeaderMap, name: &str, value: &str) {
    if let (Ok(header_name), Ok(header_value)) =
        (name.parse::<HeaderName>(), HeaderValue::from_str(value))
    {
        headers.insert(header_name, header_value);
    }
}

fn copy_if_present(source: &HeaderMap, dest: &mut HeaderMap, name: &str) {
    if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
        for value in source.get_all(&header_name) {
            dest.append(header_name.clone(), value.clone());
        }
    }
}

pub(crate) fn sanitize_response_headers(headers: &HeaderMap) -> HeaderMap {
    let mut cloned = headers.clone();
    remove_hop_by_hop_response_headers(&mut cloned);
    cloned.remove(CONTENT_ENCODING);
    cloned.remove(CONTENT_LENGTH);
    normalize_rate_limit_headers(&mut cloned);
    cloned
}

fn remove_hop_by_hop_response_headers(headers: &mut HeaderMap) {
    let connection_header_values = headers
        .get_all(CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok().map(str::to_owned))
        .collect::<Vec<_>>();

    for raw_value in connection_header_values {
        for token in raw_value.split(',') {
            let name = token.trim();
            if name.is_empty() {
                continue;
            }
            if let Ok(header_name) = HeaderName::from_bytes(name.as_bytes()) {
                headers.remove(header_name);
            }
        }
    }

    headers.remove(CONNECTION);
    headers.remove(KEEP_ALIVE_HEADER);
    headers.remove(PROXY_AUTHENTICATE);
    headers.remove(PROXY_AUTHORIZATION);
    headers.remove(TE);
    headers.remove(TRAILER);
    headers.remove(TRANSFER_ENCODING);
    headers.remove(UPGRADE);
}

fn normalize_rate_limit_headers(headers: &mut HeaderMap) {
    let names = headers
        .keys()
        .map(|name| name.as_str().to_string())
        .collect::<Vec<_>>();
    for name in names {
        let lowered = name.to_ascii_lowercase();
        if !(lowered.ends_with("-primary-used-percent")
            || lowered.ends_with("-secondary-used-percent"))
        {
            continue;
        }
        if let Ok(value) = HeaderValue::from_str("0.0")
            && let Ok(header_name) = HeaderName::from_bytes(name.as_bytes())
        {
            headers.insert(header_name, value);
        }
    }
}

#[cfg(test)]
pub(super) fn parse_retry_after_header(headers: &HeaderMap) -> Option<std::time::Duration> {
    crate::error_semantics::parse_retry_after_header(Some(headers))
}
