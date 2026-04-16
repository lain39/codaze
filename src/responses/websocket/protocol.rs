use super::errors::{
    WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE, WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE,
};
use crate::config::FingerprintMode;
use crate::models::ModelsSnapshot;
use crate::request_normalization::normalize_responses_request_body;
use crate::upstream::fingerprint::{
    apply_client_metadata_installation_id, apply_client_metadata_user_thread_source,
};
use axum::extract::ws::{CloseFrame as AxumCloseFrame, Message as AxumWsMessage};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

pub(crate) fn normalize_websocket_rate_limit_message(
    message: TungsteniteMessage,
    codex_originator: bool,
) -> Option<TungsteniteMessage> {
    let TungsteniteMessage::Text(text) = message else {
        return Some(message);
    };
    if websocket_message_type(text.as_ref()).as_deref() != Some("codex.rate_limits") {
        return Some(TungsteniteMessage::Text(text));
    }
    if !codex_originator {
        return None;
    }
    let Some(normalized) = normalize_rate_limit_event_payload(text.as_ref()) else {
        return Some(TungsteniteMessage::Text(text));
    };
    Some(TungsteniteMessage::Text(normalized.into()))
}

pub(crate) fn rewrite_previous_response_not_found_message(
    message: TungsteniteMessage,
) -> TungsteniteMessage {
    let TungsteniteMessage::Text(text) = message else {
        return message;
    };
    let Some(rewritten) = rewrite_previous_response_not_found_payload(text.as_ref()) else {
        return TungsteniteMessage::Text(text);
    };
    TungsteniteMessage::Text(rewritten.into())
}

pub(crate) fn normalize_response_create_message(
    message: TungsteniteMessage,
    mode: FingerprintMode,
    codex_originator: bool,
    snapshot: Option<&ModelsSnapshot>,
    installation_id: Option<&str>,
) -> TungsteniteMessage {
    let TungsteniteMessage::Text(text) = message else {
        return message;
    };
    let Some(normalized) = normalize_response_create_payload(
        text.as_ref(),
        mode,
        codex_originator,
        snapshot,
        installation_id,
    ) else {
        return TungsteniteMessage::Text(text);
    };
    TungsteniteMessage::Text(normalized.into())
}

pub(crate) fn normalize_response_create_payload(
    text: &str,
    mode: FingerprintMode,
    codex_originator: bool,
    snapshot: Option<&ModelsSnapshot>,
    installation_id: Option<&str>,
) -> Option<String> {
    let mut json = serde_json::from_str::<Value>(text).ok()?;
    if json.get("type").and_then(Value::as_str) != Some("response.create") {
        return None;
    }

    let original = json.clone();
    normalize_responses_request_body(mode, codex_originator, &mut json, snapshot);

    if mode == FingerprintMode::Normalize {
        let _ = apply_client_metadata_user_thread_source(&mut json);
        if let Some(installation_id) = installation_id {
            let _ = apply_client_metadata_installation_id(&mut json, installation_id);
        }
    }

    if json == original {
        return None;
    }
    serde_json::to_string(&json).ok()
}

#[cfg(test)]
pub(crate) fn normalize_response_create_installation_id_payload(
    text: &str,
    mode: FingerprintMode,
    installation_id: Option<&str>,
) -> Option<String> {
    normalize_response_create_payload(text, mode, true, None, installation_id)
}

// Intentionally map upstream previous_response_not_found into Codex's
// websocket_connection_limit_reached retry/reset path. That makes the next
// create fall back to a full request without previous_response_id instead of
// surfacing the upstream incremental-state mismatch directly to the client.
pub(crate) fn rewrite_previous_response_not_found_payload(text: &str) -> Option<String> {
    let json = serde_json::from_str::<Value>(text).ok()?;
    if json.get("type").and_then(Value::as_str) != Some("error") {
        return None;
    }
    if json
        .get("error")
        .and_then(|error| error.get("code"))
        .and_then(Value::as_str)
        != Some("previous_response_not_found")
    {
        return None;
    }
    serde_json::to_string(&json!({
        "type": "error",
        "status": 400,
        "error": {
            "type": "invalid_request_error",
            "code": WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE,
            "message": WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE,
        }
    }))
    .ok()
}

pub(crate) fn should_passthrough_retryable_websocket_reset(
    has_pending_uncommitted_request: bool,
    message: &TungsteniteMessage,
) -> bool {
    has_pending_uncommitted_request
        && websocket_wrapped_error_has_code(message, WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE)
}

fn websocket_wrapped_error_has_code(message: &TungsteniteMessage, expected_code: &str) -> bool {
    let TungsteniteMessage::Text(text) = message else {
        return false;
    };
    serde_json::from_str::<Value>(text.as_ref())
        .ok()
        .and_then(|json| json.get("error").cloned())
        .and_then(|error| error.get("code").cloned())
        .and_then(|code| code.as_str().map(str::to_string))
        .is_some_and(|code| code == expected_code)
}

pub(crate) fn normalize_rate_limit_event_payload(text: &str) -> Option<String> {
    let mut json = serde_json::from_str::<Value>(text).ok()?;
    if json.get("type").and_then(Value::as_str) != Some("codex.rate_limits") {
        return None;
    }
    let rate_limits = json.get_mut("rate_limits")?.as_object_mut()?;
    normalize_rate_limit_window_used_percent(rate_limits.get_mut("primary"));
    normalize_rate_limit_window_used_percent(rate_limits.get_mut("secondary"));
    serde_json::to_string(&json).ok()
}

fn normalize_rate_limit_window_used_percent(window: Option<&mut Value>) {
    let Some(window) = window.and_then(Value::as_object_mut) else {
        return;
    };
    window.insert("used_percent".to_string(), Value::from(0.0));
}

pub(crate) fn is_responses_websocket_request_start(message: &TungsteniteMessage) -> bool {
    matches!(
        message,
        TungsteniteMessage::Text(text)
            if websocket_message_type(text.as_ref()).as_deref() == Some("response.create")
    )
}

pub(super) fn should_replay_client_message(message: &TungsteniteMessage) -> bool {
    matches!(
        message,
        TungsteniteMessage::Text(_) | TungsteniteMessage::Binary(_)
    )
}

pub(super) fn should_buffer_upstream_message_before_commit(message: &TungsteniteMessage) -> bool {
    matches!(
        message,
        TungsteniteMessage::Text(_) | TungsteniteMessage::Binary(_)
    )
}

pub(crate) fn upstream_message_commits_request(message: &TungsteniteMessage) -> bool {
    match message {
        TungsteniteMessage::Text(text) => match websocket_message_type(text.as_ref()).as_deref() {
            Some("error" | "response.failed" | "response.incomplete" | "codex.rate_limits") => {
                false
            }
            Some(_) | None => true,
        },
        TungsteniteMessage::Binary(_) => true,
        TungsteniteMessage::Ping(_)
        | TungsteniteMessage::Pong(_)
        | TungsteniteMessage::Frame(_) => false,
        TungsteniteMessage::Close(_) => false,
    }
}

pub(crate) fn upstream_message_is_terminal(message: &TungsteniteMessage) -> bool {
    match message {
        TungsteniteMessage::Text(text) => matches!(
            websocket_message_type(text.as_ref()).as_deref(),
            Some("error" | "response.failed" | "response.incomplete" | "response.completed")
        ),
        TungsteniteMessage::Close(_) => true,
        _ => false,
    }
}

pub(super) fn websocket_message_type(text: &str) -> Option<String> {
    serde_json::from_str::<Value>(text)
        .ok()?
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(super) fn map_client_message_to_upstream(message: AxumWsMessage) -> Option<TungsteniteMessage> {
    match message {
        AxumWsMessage::Text(text) => Some(TungsteniteMessage::Text(text.to_string().into())),
        AxumWsMessage::Binary(bytes) => Some(TungsteniteMessage::Binary(bytes)),
        AxumWsMessage::Ping(bytes) => Some(TungsteniteMessage::Ping(bytes)),
        AxumWsMessage::Pong(bytes) => Some(TungsteniteMessage::Pong(bytes)),
        AxumWsMessage::Close(frame) => Some(TungsteniteMessage::Close(
            frame.and_then(sanitize_client_close_frame_for_upstream),
        )),
    }
}

pub(super) fn map_upstream_message_to_client(message: TungsteniteMessage) -> Option<AxumWsMessage> {
    match message {
        TungsteniteMessage::Text(text) => Some(AxumWsMessage::Text(text.to_string().into())),
        TungsteniteMessage::Binary(bytes) => Some(AxumWsMessage::Binary(bytes)),
        TungsteniteMessage::Ping(bytes) => Some(AxumWsMessage::Ping(bytes)),
        TungsteniteMessage::Pong(bytes) => Some(AxumWsMessage::Pong(bytes)),
        TungsteniteMessage::Close(frame) => Some(AxumWsMessage::Close(
            frame.and_then(sanitize_upstream_close_frame_for_client),
        )),
        TungsteniteMessage::Frame(_) => None,
    }
}

fn sanitize_client_close_frame_for_upstream(
    frame: AxumCloseFrame,
) -> Option<TungsteniteCloseFrame> {
    let code = CloseCode::from(frame.code);
    is_wire_legal_close_code(code).then(|| TungsteniteCloseFrame {
        code,
        reason: frame.reason.to_string().into(),
    })
}

fn sanitize_upstream_close_frame_for_client(
    frame: TungsteniteCloseFrame,
) -> Option<AxumCloseFrame> {
    is_wire_legal_close_code(frame.code).then(|| AxumCloseFrame {
        code: u16::from(frame.code),
        reason: frame.reason.to_string().into(),
    })
}

fn is_wire_legal_close_code(code: CloseCode) -> bool {
    !matches!(
        code,
        CloseCode::Status
            | CloseCode::Abnormal
            | CloseCode::Tls
            | CloseCode::Reserved(_)
            | CloseCode::Bad(_)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_client_close_drops_illegal_wire_code() {
        let message = AxumWsMessage::Close(Some(AxumCloseFrame {
            code: 1006,
            reason: "".into(),
        }));

        let Some(TungsteniteMessage::Close(frame)) = map_client_message_to_upstream(message) else {
            panic!("expected close frame");
        };
        assert!(frame.is_none());
    }

    #[test]
    fn map_client_close_drops_bad_wire_code() {
        let message = AxumWsMessage::Close(Some(AxumCloseFrame {
            code: 1004,
            reason: "".into(),
        }));

        let Some(TungsteniteMessage::Close(frame)) = map_client_message_to_upstream(message) else {
            panic!("expected close frame");
        };
        assert!(frame.is_none());
    }

    #[test]
    fn map_upstream_close_drops_illegal_wire_code() {
        let message = TungsteniteMessage::Close(Some(TungsteniteCloseFrame {
            code: CloseCode::Abnormal,
            reason: "".into(),
        }));

        let Some(AxumWsMessage::Close(frame)) = map_upstream_message_to_client(message) else {
            panic!("expected close frame");
        };
        assert!(frame.is_none());
    }

    #[test]
    fn map_upstream_close_drops_reserved_wire_code() {
        let message = TungsteniteMessage::Close(Some(TungsteniteCloseFrame {
            code: CloseCode::Reserved(1016),
            reason: "".into(),
        }));

        let Some(AxumWsMessage::Close(frame)) = map_upstream_message_to_client(message) else {
            panic!("expected close frame");
        };
        assert!(frame.is_none());
    }
}
