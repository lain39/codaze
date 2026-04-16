use crate::classifier::FailureClass;
use chrono::{DateTime, Utc};
use http::{HeaderMap, StatusCode};
use serde_json::Value;
use std::time::Duration;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StructuredErrorPayload {
    pub(crate) code: Option<String>,
    pub(crate) message: Option<String>,
    pub(crate) error_type: Option<String>,
    pub(crate) plan_type: Option<String>,
    pub(crate) resets_at: Option<i64>,
    pub(crate) resets_in_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamErrorSemantics {
    pub(crate) failure: FailureClass,
    pub(crate) retry_after: Option<Duration>,
    pub(crate) payload: StructuredErrorPayload,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct AnalyzeErrorContext<'a> {
    pub(crate) status: StatusCode,
    pub(crate) headers: Option<&'a HeaderMap>,
    pub(crate) body: Option<&'a str>,
    pub(crate) explicit_code: Option<&'a str>,
    pub(crate) explicit_message: Option<&'a str>,
    pub(crate) explicit_error_type: Option<&'a str>,
    pub(crate) explicit_plan_type: Option<&'a str>,
    pub(crate) explicit_resets_at: Option<i64>,
    pub(crate) explicit_resets_in_seconds: Option<i64>,
    pub(crate) explicit_retry_after: Option<Duration>,
    pub(crate) unauthorized_failure: FailureClass,
    pub(crate) allow_message_retry_after: bool,
}

pub(crate) fn analyze_request_http(
    status: StatusCode,
    headers: Option<&HeaderMap>,
    body: Option<&str>,
) -> UpstreamErrorSemantics {
    analyze_error(AnalyzeErrorContext {
        status,
        headers,
        body,
        explicit_code: None,
        explicit_message: None,
        explicit_error_type: None,
        explicit_plan_type: None,
        explicit_resets_at: None,
        explicit_resets_in_seconds: None,
        explicit_retry_after: None,
        unauthorized_failure: FailureClass::AccessTokenRejected,
        allow_message_retry_after: true,
    })
}

pub(crate) fn analyze_refresh_http(
    status: StatusCode,
    headers: Option<&HeaderMap>,
    body: Option<&str>,
) -> UpstreamErrorSemantics {
    let mut semantics = analyze_error(AnalyzeErrorContext {
        status,
        headers,
        body,
        explicit_code: None,
        explicit_message: None,
        explicit_error_type: None,
        explicit_plan_type: None,
        explicit_resets_at: None,
        explicit_resets_in_seconds: None,
        explicit_retry_after: None,
        unauthorized_failure: FailureClass::AuthInvalid,
        allow_message_retry_after: true,
    });
    if semantics.failure == FailureClass::RequestRejected
        && body.is_some_and(refresh_body_indicates_invalid_token)
    {
        semantics.failure = FailureClass::AuthInvalid;
    }
    semantics
}

pub(crate) fn analyze_error(context: AnalyzeErrorContext<'_>) -> UpstreamErrorSemantics {
    let mut payload = parse_structured_error_body(context.body);
    if let Some(code) = context.explicit_code {
        payload.code = Some(code.to_owned());
    }
    if let Some(message) = context.explicit_message {
        payload.message = Some(message.to_owned());
    }
    if let Some(error_type) = context.explicit_error_type {
        payload.error_type = Some(error_type.to_owned());
    }
    if let Some(plan_type) = context.explicit_plan_type {
        payload.plan_type = Some(plan_type.to_owned());
    }
    if let Some(resets_at) = context.explicit_resets_at {
        payload.resets_at = Some(resets_at);
    }
    if let Some(resets_in_seconds) = context.explicit_resets_in_seconds {
        payload.resets_in_seconds = Some(resets_in_seconds);
    }

    let fallback_text = context.body.or(payload.message.as_deref());
    let failure = classify_error(
        context.status,
        payload.code.as_deref(),
        payload.error_type.as_deref(),
        fallback_text,
        context.unauthorized_failure,
    );
    let retry_after = context
        .explicit_retry_after
        .or_else(|| parse_retry_after_header(context.headers))
        .or_else(|| parse_resets_retry_after_payload(&payload))
        .or_else(|| {
            if context.allow_message_retry_after {
                payload
                    .message
                    .as_deref()
                    .or(context.body)
                    .and_then(parse_retry_after_message)
            } else {
                None
            }
        });

    UpstreamErrorSemantics {
        failure,
        retry_after,
        payload,
    }
}

pub(crate) fn parse_structured_error_body(body: Option<&str>) -> StructuredErrorPayload {
    let Some(body) = body else {
        return StructuredErrorPayload::default();
    };
    let Ok(json) = serde_json::from_str::<Value>(body) else {
        return StructuredErrorPayload::default();
    };
    parse_structured_error_value(json.get("error").unwrap_or(&json))
}

pub(crate) fn parse_structured_error_value(value: &Value) -> StructuredErrorPayload {
    StructuredErrorPayload {
        code: value
            .get("code")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        error_type: value
            .get("type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        plan_type: value
            .get("plan_type")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
        resets_at: value.get("resets_at").and_then(Value::as_i64),
        resets_in_seconds: value.get("resets_in_seconds").and_then(Value::as_i64),
    }
}

pub(crate) fn parse_retry_after_str(value: &str) -> Option<Duration> {
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    let parsed = DateTime::parse_from_rfc2822(value).ok()?;
    let delta = parsed.with_timezone(&Utc) - Utc::now();
    delta.to_std().ok()
}

pub(crate) fn parse_retry_after_header(headers: Option<&HeaderMap>) -> Option<Duration> {
    let value = headers?.get("retry-after")?.to_str().ok()?.trim();
    parse_retry_after_str(value)
}

pub(crate) fn parse_resets_retry_after_payload(
    payload: &StructuredErrorPayload,
) -> Option<Duration> {
    if let Some(resets_at) = payload.resets_at {
        let now = Utc::now().timestamp();
        if resets_at > now {
            return Some(Duration::from_secs((resets_at - now) as u64));
        }
    }
    if let Some(seconds) = payload.resets_in_seconds
        && seconds > 0
    {
        return Some(Duration::from_secs(seconds as u64));
    }
    None
}

fn classify_error(
    status: StatusCode,
    code: Option<&str>,
    error_type: Option<&str>,
    fallback_text: Option<&str>,
    unauthorized_failure: FailureClass,
) -> FailureClass {
    if status == StatusCode::UNAUTHORIZED {
        return unauthorized_failure;
    }
    if let Some(failure) = classify_structured_error(code, error_type) {
        return failure;
    }
    classify_http_from_status_and_text(status, fallback_text)
}

fn classify_structured_error(code: Option<&str>, error_type: Option<&str>) -> Option<FailureClass> {
    match (code, error_type) {
        (_, Some("usage_limit_reached")) => Some(FailureClass::QuotaExhausted),
        (_, Some("usage_not_included")) => Some(FailureClass::RequestRejected),
        (Some("rate_limit_exceeded"), _) => Some(FailureClass::RateLimited),
        (Some("insufficient_quota"), _) => Some(FailureClass::QuotaExhausted),
        (Some("websocket_connection_limit_reached"), _) => Some(FailureClass::TemporaryFailure),
        (Some("server_is_overloaded" | "slow_down"), _) => Some(FailureClass::TemporaryFailure),
        (Some("invalid_prompt" | "context_length_exceeded"), _) => {
            Some(FailureClass::RequestRejected)
        }
        _ => None,
    }
}

fn classify_http_from_status_and_text(status: StatusCode, body: Option<&str>) -> FailureClass {
    let body = body.unwrap_or_default().to_ascii_lowercase();

    if status == StatusCode::FORBIDDEN
        && (body.contains("unusual activity")
            || body.contains("arkose")
            || body.contains("turnstile"))
    {
        return FailureClass::RiskControlled;
    }
    if status == StatusCode::TOO_MANY_REQUESTS || body.contains("rate limit") {
        return FailureClass::RateLimited;
    }
    if body.contains("quota")
        || body.contains("credits")
        || body.contains("usage cap")
        || body.contains("limit reached")
    {
        return FailureClass::QuotaExhausted;
    }
    if status == StatusCode::FORBIDDEN {
        return FailureClass::TemporaryFailure;
    }
    if status.is_server_error() {
        return FailureClass::TemporaryFailure;
    }
    FailureClass::RequestRejected
}

fn parse_retry_after_message(message: &str) -> Option<Duration> {
    let lowered = message.to_ascii_lowercase();
    let marker = "try again in";
    let start = lowered.find(marker)? + marker.len();
    let rest = lowered.get(start..)?.trim_start();
    let mut value = String::new();
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.peek() {
        if ch.is_ascii_digit() || *ch == '.' {
            value.push(*ch);
            chars.next();
        } else {
            break;
        }
    }
    if value.is_empty() {
        return None;
    }
    let unit = chars.collect::<String>().trim_start().to_string();
    let seconds = value.parse::<f64>().ok()?;
    let multiplier =
        if unit.starts_with("millisecond") || unit.starts_with("msec") || unit.starts_with("ms") {
            0.001
        } else if unit.starts_with('m') {
            60.0
        } else if unit.starts_with('h') {
            3600.0
        } else {
            1.0
        };
    Duration::try_from_secs_f64(seconds * multiplier).ok()
}

fn refresh_body_indicates_invalid_token(body: &str) -> bool {
    let lowered = body.to_ascii_lowercase();
    lowered.contains("refresh_token_expired")
        || lowered.contains("invalid refresh token")
        || lowered.contains("could not validate your refresh token")
        || lowered.contains("\"invalid_grant\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::HeaderValue;

    #[test]
    fn request_analysis_prefers_structured_invalid_prompt_over_status() {
        let semantics = analyze_request_http(
            StatusCode::BAD_GATEWAY,
            None,
            Some(r#"{"error":{"code":"invalid_prompt","message":"bad prompt"}}"#),
        );

        assert_eq!(semantics.failure, FailureClass::RequestRejected);
    }

    #[test]
    fn refresh_analysis_uses_body_retry_after_when_header_missing() {
        let semantics = analyze_refresh_http(
            StatusCode::BAD_GATEWAY,
            None,
            Some(
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#,
            ),
        );

        assert_eq!(semantics.failure, FailureClass::QuotaExhausted);
        assert_eq!(semantics.retry_after, Some(Duration::from_secs(77)));
    }

    #[test]
    fn refresh_analysis_prefers_header_retry_after_over_body_resets() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", HeaderValue::from_static("11"));
        let semantics = analyze_refresh_http(
            StatusCode::TOO_MANY_REQUESTS,
            Some(&headers),
            Some(
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached","resets_in_seconds":77}}"#,
            ),
        );

        assert_eq!(semantics.retry_after, Some(Duration::from_secs(11)));
    }

    #[test]
    fn refresh_analysis_400_invalid_refresh_token_is_auth_invalid() {
        let semantics = analyze_refresh_http(
            StatusCode::BAD_REQUEST,
            None,
            Some(
                r#"{"error":{"message":"Could not validate your refresh token. Please try signing in again.","type":"invalid_request_error","code":"refresh_token_expired"}}"#,
            ),
        );

        assert_eq!(semantics.failure, FailureClass::AuthInvalid);
    }

    #[test]
    fn explicit_message_retry_after_is_used_for_wrapped_errors() {
        let semantics = analyze_error(AnalyzeErrorContext {
            status: StatusCode::TOO_MANY_REQUESTS,
            headers: None,
            body: None,
            explicit_code: Some("rate_limit_exceeded"),
            explicit_message: Some("Rate limit reached. Please try again in 8s."),
            explicit_error_type: Some("invalid_request_error"),
            explicit_plan_type: None,
            explicit_resets_at: None,
            explicit_resets_in_seconds: None,
            explicit_retry_after: None,
            unauthorized_failure: FailureClass::AccessTokenRejected,
            allow_message_retry_after: true,
        });

        assert_eq!(semantics.failure, FailureClass::RateLimited);
        assert_eq!(semantics.retry_after, Some(Duration::from_secs(8)));
    }

    #[test]
    fn request_analysis_uses_message_retry_after_when_header_and_body_resets_are_missing() {
        let semantics = analyze_request_http(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            Some(
                r#"{"error":{"message":"Rate limit reached for gpt-5.4. Please try again in 8s.","code":"rate_limit_exceeded"}}"#,
            ),
        );

        assert_eq!(semantics.failure, FailureClass::RateLimited);
        assert_eq!(semantics.retry_after, Some(Duration::from_secs(8)));
    }

    #[test]
    fn request_analysis_plain_forbidden_is_temporary_failure() {
        let semantics = analyze_request_http(StatusCode::FORBIDDEN, None, Some("forbidden"));

        assert_eq!(semantics.failure, FailureClass::TemporaryFailure);
        assert_eq!(semantics.retry_after, None);
    }

    #[test]
    fn request_analysis_message_retry_after_supports_milliseconds() {
        let semantics = analyze_request_http(
            StatusCode::TOO_MANY_REQUESTS,
            None,
            Some(
                r#"{"error":{"message":"Rate limit reached for gpt-5.4. Please try again in 500 milliseconds.","code":"rate_limit_exceeded"}}"#,
            ),
        );

        assert_eq!(semantics.failure, FailureClass::RateLimited);
        assert_eq!(semantics.retry_after, Some(Duration::from_millis(500)));
    }
}
