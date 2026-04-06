use super::WebsocketProxyOutcome;
use crate::accounts::PoolBlockSummary;
use crate::classifier::FailureClass;
use crate::error_semantics::{AnalyzeErrorContext, analyze_error, parse_structured_error_value};
use crate::failover::FailoverFailure;
use crate::gateway_errors::CLIENT_NO_ACCOUNT_AVAILABLE_MESSAGE;
use crate::responses::failure::{
    SyntheticResponseFailedPayload, extract_retry_after, fallback_error_code_for_http,
    synthetic_response_failed_payload_from_http_failure,
    synthetic_response_failed_payload_from_transport,
};
use http::{HeaderValue, StatusCode};
use serde_json::{Value, json};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use tokio_tungstenite::tungstenite::protocol::CloseFrame as TungsteniteCloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

pub(crate) const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str =
    "websocket_connection_limit_reached";
pub(crate) const WEBSOCKET_CONNECTION_LIMIT_REACHED_MESSAGE: &str = "Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue.";

pub(crate) fn classify_websocket_upstream_message(
    message: &TungsteniteMessage,
) -> Option<WebsocketProxyOutcome> {
    match message {
        TungsteniteMessage::Text(text) => classify_websocket_error_text(text.as_ref()),
        TungsteniteMessage::Close(Some(frame)) => Some(classify_websocket_close_frame(frame)),
        TungsteniteMessage::Close(None) => Some(WebsocketProxyOutcome::Released),
        _ => None,
    }
}

pub(crate) fn websocket_error_message_for_failover_failure(
    failure: &FailoverFailure,
) -> Option<TungsteniteMessage> {
    match failure {
        FailoverFailure::PoolBlocked(summary) => websocket_error_message_for_pool_block(summary),
        FailoverFailure::Refresh(error) => websocket_error_message_from_payload(
            error.status,
            synthetic_response_failed_payload_from_http_failure(
                error.status,
                Some(error.body.as_str()),
                error.retry_after,
            ),
            error.retry_after,
        ),
        FailoverFailure::Transport(error) => websocket_error_message_from_transport_error(error),
        FailoverFailure::Json { status, message } => websocket_error_message_from_payload(
            *status,
            SyntheticResponseFailedPayload {
                code: Some(fallback_error_code_for_http(*status, None).to_string()),
                message: Some(message.clone()),
                error_type: None,
                plan_type: None,
                resets_at: None,
                resets_in_seconds: None,
            },
            None,
        ),
    }
}

fn websocket_error_message_for_pool_block(
    summary: &PoolBlockSummary,
) -> Option<TungsteniteMessage> {
    let _ = summary;
    let mut error_object = serde_json::Map::new();
    error_object.insert(
        "code".to_string(),
        Value::String("server_is_overloaded".to_string()),
    );
    error_object.insert(
        "message".to_string(),
        Value::String(CLIENT_NO_ACCOUNT_AVAILABLE_MESSAGE.to_string()),
    );

    let payload = serde_json::Map::from_iter([
        ("type".to_string(), Value::String("error".to_string())),
        (
            "status".to_string(),
            Value::Number(serde_json::Number::from(
                StatusCode::SERVICE_UNAVAILABLE.as_u16(),
            )),
        ),
        ("error".to_string(), Value::Object(error_object)),
    ]);

    serde_json::to_string(&Value::Object(payload))
        .ok()
        .map(|text| TungsteniteMessage::Text(text.into()))
}

fn websocket_error_message_from_transport_error(
    error: &codex_client::TransportError,
) -> Option<TungsteniteMessage> {
    let status = match error {
        codex_client::TransportError::Http { status, .. } => *status,
        codex_client::TransportError::Timeout => StatusCode::REQUEST_TIMEOUT,
        codex_client::TransportError::Network(_)
        | codex_client::TransportError::Build(_)
        | codex_client::TransportError::RetryLimit => StatusCode::BAD_GATEWAY,
    };
    websocket_error_message_from_payload(
        status,
        synthetic_response_failed_payload_from_transport(error),
        extract_retry_after(error),
    )
}

fn websocket_error_message_from_payload(
    status: StatusCode,
    payload: SyntheticResponseFailedPayload,
    retry_after: Option<Duration>,
) -> Option<TungsteniteMessage> {
    let mut error_object = serde_json::Map::new();
    error_object.insert(
        "code".to_string(),
        Value::String(
            payload
                .code
                .unwrap_or_else(|| fallback_error_code_for_http(status, None).to_string()),
        ),
    );
    error_object.insert(
        "message".to_string(),
        Value::String(
            payload
                .message
                .unwrap_or_else(|| format!("Upstream request failed with status {}.", status)),
        ),
    );
    if let Some(error_type) = payload.error_type {
        error_object.insert("type".to_string(), Value::String(error_type));
    }
    if let Some(plan_type) = payload.plan_type {
        error_object.insert("plan_type".to_string(), Value::String(plan_type));
    }
    if let Some(resets_at) = payload.resets_at {
        error_object.insert(
            "resets_at".to_string(),
            Value::Number(serde_json::Number::from(resets_at)),
        );
    }
    if let Some(resets_in_seconds) = payload.resets_in_seconds {
        error_object.insert(
            "resets_in_seconds".to_string(),
            Value::Number(serde_json::Number::from(resets_in_seconds)),
        );
    }

    let mut message = serde_json::Map::from_iter([
        ("type".to_string(), Value::String("error".to_string())),
        (
            "status".to_string(),
            Value::Number(serde_json::Number::from(status.as_u16())),
        ),
        ("error".to_string(), Value::Object(error_object)),
    ]);
    if let Some(retry_after) = retry_after {
        let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
        message.insert(
            "headers".to_string(),
            json!({ "retry-after": seconds.max(1).to_string() }),
        );
    }

    serde_json::to_string(&Value::Object(message))
        .ok()
        .map(|text| TungsteniteMessage::Text(text.into()))
}

pub(crate) fn classify_websocket_error_text(text: &str) -> Option<WebsocketProxyOutcome> {
    let json = serde_json::from_str::<Value>(text).ok()?;
    let event_type = json.get("type").and_then(Value::as_str);
    if event_type == Some("error") {
        let status = json
            .get("status")
            .and_then(Value::as_u64)
            .and_then(|value| u16::try_from(value).ok())
            .and_then(|value| StatusCode::from_u16(value).ok())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let explicit_error_type = json
            .get("error")
            .and_then(|error| error.get("type"))
            .and_then(Value::as_str);
        let explicit_code = json
            .get("error")
            .and_then(|error| error.get("code"))
            .and_then(Value::as_str);
        let explicit_message = json
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(Value::as_str);
        let explicit_plan_type = json
            .get("error")
            .and_then(|error| error.get("plan_type"))
            .and_then(Value::as_str);
        let explicit_resets_at = json
            .get("error")
            .and_then(|error| error.get("resets_at"))
            .and_then(Value::as_i64);
        let explicit_resets_in_seconds = json
            .get("error")
            .and_then(|error| error.get("resets_in_seconds"))
            .and_then(Value::as_i64);
        let explicit_retry_after = json
            .get("headers")
            .and_then(Value::as_object)
            .and_then(|headers| headers.get("retry-after"))
            .and_then(json_header_value_from_json)
            .as_ref()
            .and_then(parse_retry_after);
        let semantics = analyze_error(AnalyzeErrorContext {
            status,
            headers: None,
            body: None,
            explicit_code,
            explicit_message,
            explicit_error_type,
            explicit_plan_type,
            explicit_resets_at,
            explicit_resets_in_seconds,
            explicit_retry_after,
            unauthorized_failure: FailureClass::AccessTokenRejected,
            allow_message_retry_after: true,
        });
        return Some(WebsocketProxyOutcome::Failed {
            failure: semantics.failure,
            retry_after: semantics.retry_after,
            details: describe_wrapped_websocket_error_event(&json, status),
        });
    }

    let event_type = event_type?;
    if event_type != "response.failed" && event_type != "response.incomplete" {
        return None;
    }
    let response = json.get("response")?;
    let terminal = if event_type == "response.failed" {
        let error = response.get("error").cloned().unwrap_or(Value::Null);
        let (failure, retry_after, details) = classify_response_failed_event(&error);
        WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            details: format!("responses websocket upstream returned response.failed: {details}"),
        }
    } else {
        let reason = response
            .get("incomplete_details")
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        WebsocketProxyOutcome::Failed {
            failure: FailureClass::RequestRejected,
            retry_after: None,
            details: format!(
                "responses websocket upstream returned response.incomplete: reason={reason}"
            ),
        }
    };
    Some(terminal)
}

fn classify_websocket_close_frame(frame: &TungsteniteCloseFrame) -> WebsocketProxyOutcome {
    let outcome = classify_websocket_close_reason(frame.reason.as_ref());
    if let Some(outcome) = outcome {
        return outcome;
    }
    if is_normal_websocket_close_code(frame.code) {
        WebsocketProxyOutcome::Released
    } else {
        WebsocketProxyOutcome::Failed {
            failure: FailureClass::TemporaryFailure,
            retry_after: None,
            details: format!(
                "responses websocket upstream closed with error: code={} reason={}",
                frame.code, frame.reason
            ),
        }
    }
}

fn classify_websocket_close_reason(reason: &str) -> Option<WebsocketProxyOutcome> {
    let lowered = reason.to_ascii_lowercase();
    let failure = if lowered.contains("unusual activity")
        || lowered.contains("arkose")
        || lowered.contains("turnstile")
    {
        FailureClass::RiskControlled
    } else if lowered.contains("rate limit") {
        FailureClass::RateLimited
    } else if lowered.contains("quota") || lowered.contains("usage cap") {
        FailureClass::QuotaExhausted
    } else {
        return None;
    };
    Some(WebsocketProxyOutcome::Failed {
        failure,
        retry_after: None,
        details: format!("responses websocket upstream closed with error: reason={reason}"),
    })
}

fn describe_wrapped_websocket_error_event(payload: &Value, status: StatusCode) -> String {
    let mut segments = vec![format!("status={}", status.as_u16())];
    if let Some(error) = payload.get("error") {
        push_json_string_field(&mut segments, "error.type", error.get("type"));
        push_json_string_field(&mut segments, "error.code", error.get("code"));
        push_json_string_field(&mut segments, "error.message", error.get("message"));
        push_json_integer_field(&mut segments, "error.resets_at", error.get("resets_at"));
        push_json_integer_field(
            &mut segments,
            "error.resets_in_seconds",
            error.get("resets_in_seconds"),
        );
    }
    if let Some(retry_after) = payload
        .get("headers")
        .and_then(Value::as_object)
        .and_then(|headers| headers.get("retry-after"))
    {
        push_json_display_field(&mut segments, "headers.retry-after", Some(retry_after));
    }
    format!(
        "responses websocket upstream returned error event: {}",
        segments.join(" ")
    )
}

fn push_json_string_field(segments: &mut Vec<String>, name: &str, value: Option<&Value>) {
    let Some(value) = value.and_then(Value::as_str) else {
        return;
    };
    segments.push(format!("{name}={value}"));
}

fn push_json_integer_field(segments: &mut Vec<String>, name: &str, value: Option<&Value>) {
    let Some(value) = value.and_then(Value::as_i64) else {
        return;
    };
    segments.push(format!("{name}={value}"));
}

fn push_json_display_field(segments: &mut Vec<String>, name: &str, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    let rendered = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => return,
    };
    segments.push(format!("{name}={rendered}"));
}

fn is_normal_websocket_close_code(code: CloseCode) -> bool {
    matches!(
        code,
        CloseCode::Normal | CloseCode::Away | CloseCode::Restart | CloseCode::Again
    )
}

fn json_header_value_from_json(value: &Value) -> Option<HeaderValue> {
    let value = match value {
        Value::String(value) => value.clone(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => value.to_string(),
        _ => return None,
    };
    HeaderValue::from_str(&value).ok()
}

pub(crate) fn classify_response_failed_event(
    error: &Value,
) -> (FailureClass, Option<Duration>, String) {
    let details = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("response.failed event received")
        .to_string();
    let code = error.get("code").and_then(Value::as_str);
    let error_type = error.get("type").and_then(Value::as_str);
    let status = infer_response_error_status(code, error_type, &details);
    let payload = parse_structured_error_value(error);
    let semantics = analyze_error(AnalyzeErrorContext {
        status,
        headers: None,
        body: None,
        explicit_code: code,
        explicit_message: payload.message.as_deref().or(Some(details.as_str())),
        explicit_error_type: error_type,
        explicit_plan_type: payload.plan_type.as_deref(),
        explicit_resets_at: payload.resets_at,
        explicit_resets_in_seconds: payload.resets_in_seconds,
        explicit_retry_after: None,
        unauthorized_failure: FailureClass::AccessTokenRejected,
        allow_message_retry_after: true,
    });
    (semantics.failure, semantics.retry_after, details)
}

fn infer_response_error_status(
    code: Option<&str>,
    error_type: Option<&str>,
    details: &str,
) -> StatusCode {
    match (code, error_type) {
        (_, Some("usage_limit_reached")) => StatusCode::TOO_MANY_REQUESTS,
        (Some("rate_limit_exceeded"), _) => StatusCode::TOO_MANY_REQUESTS,
        (Some("insufficient_quota"), _) => StatusCode::FORBIDDEN,
        (Some("server_is_overloaded"), _) | (Some("slow_down"), _) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        (Some("invalid_prompt"), _) | (Some("context_length_exceeded"), _) => {
            StatusCode::BAD_REQUEST
        }
        _ => {
            let lowered = details.to_ascii_lowercase();
            if lowered.contains("unauthorized") {
                StatusCode::UNAUTHORIZED
            } else if lowered.contains("unusual activity")
                || lowered.contains("arkose")
                || lowered.contains("turnstile")
            {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            }
        }
    }
}

fn parse_retry_after(value: &HeaderValue) -> Option<Duration> {
    crate::error_semantics::parse_retry_after_str(value.to_str().ok()?.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounts::BlockedReason;
    use chrono::{Duration as ChronoDuration, Utc};

    #[test]
    fn websocket_error_message_for_pool_block_temporary_unavailable_is_classified() {
        let message = websocket_error_message_for_pool_block(&PoolBlockSummary {
            blocked_reason: BlockedReason::TemporarilyUnavailable,
            blocked_until: Some(Utc::now() + ChronoDuration::seconds(1)),
            retry_after: Some(Duration::from_secs(1)),
        })
        .expect("message");

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            ..
        }) = classify_websocket_upstream_message(&message)
        else {
            panic!("expected failed outcome");
        };

        assert_eq!(failure, FailureClass::TemporaryFailure);
        assert_eq!(retry_after, None);
    }

    #[test]
    fn websocket_error_message_for_pool_block_quota_is_classified() {
        let message = websocket_error_message_for_pool_block(&PoolBlockSummary {
            blocked_reason: BlockedReason::QuotaExhausted,
            blocked_until: Some(Utc::now() + ChronoDuration::minutes(10)),
            retry_after: Some(Duration::from_secs(600)),
        })
        .expect("message");

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            ..
        }) = classify_websocket_upstream_message(&message)
        else {
            panic!("expected failed outcome");
        };

        assert_eq!(failure, FailureClass::TemporaryFailure);
        assert_eq!(retry_after, None);
    }

    #[test]
    fn websocket_error_message_for_pool_block_auth_invalid_is_classified() {
        let message = websocket_error_message_for_pool_block(&PoolBlockSummary {
            blocked_reason: BlockedReason::AuthInvalid,
            blocked_until: None,
            retry_after: None,
        })
        .expect("message");

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            details,
        }) = classify_websocket_upstream_message(&message)
        else {
            panic!("expected failed outcome");
        };

        assert_eq!(failure, FailureClass::TemporaryFailure);
        assert_eq!(retry_after, None);
        assert!(details.contains("status=503"));
        assert!(details.contains("error.code=server_is_overloaded"));
    }

    #[test]
    fn websocket_error_message_for_failover_failure_http_transport_is_classified() {
        let message = websocket_error_message_for_failover_failure(&FailoverFailure::Transport(
            codex_client::TransportError::Http {
                status: StatusCode::BAD_REQUEST,
                url: None,
                headers: None,
                body: Some(
                    r#"{"error":{"type":"invalid_request_error","code":"invalid_prompt","message":"bad input"}}"#
                        .to_string(),
                ),
            },
        ))
        .expect("message");

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            details,
        }) = classify_websocket_upstream_message(&message)
        else {
            panic!("expected failed outcome");
        };

        assert_eq!(failure, FailureClass::RequestRejected);
        assert_eq!(retry_after, None);
        assert!(details.contains("status=400"));
        assert!(details.contains("error.code=invalid_prompt"));
    }

    #[test]
    fn websocket_error_message_for_failover_failure_json_internal_is_classified() {
        let message = websocket_error_message_for_failover_failure(&FailoverFailure::Json {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "disk write failed".to_string(),
        })
        .expect("message");

        let Some(WebsocketProxyOutcome::Failed {
            failure,
            retry_after,
            details,
        }) = classify_websocket_upstream_message(&message)
        else {
            panic!("expected failed outcome");
        };

        assert_eq!(failure, FailureClass::TemporaryFailure);
        assert_eq!(retry_after, None);
        assert!(details.contains("status=500"));
        assert!(details.contains("error.code=internal_server_error"));
    }
}
