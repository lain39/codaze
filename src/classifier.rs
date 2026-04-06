#[cfg(test)]
use crate::error_semantics::analyze_refresh_http;
use crate::error_semantics::analyze_request_http;
use codex_client::TransportError;
use http::StatusCode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    AccessTokenRejected,
    AuthInvalid,
    RateLimited,
    QuotaExhausted,
    RiskControlled,
    TemporaryFailure,
    RequestRejected,
    InternalFailure,
}

pub fn classify_request_error(error: &TransportError) -> FailureClass {
    match error {
        TransportError::Timeout | TransportError::Network(_) | TransportError::RetryLimit => {
            FailureClass::TemporaryFailure
        }
        TransportError::Build(_) => FailureClass::InternalFailure,
        TransportError::Http { status, body, .. } => {
            classify_request_http(*status, body.as_deref())
        }
    }
}

#[cfg(test)]
pub fn classify_refresh_response(status: StatusCode, body: &str) -> FailureClass {
    analyze_refresh_http(status, None, Some(body)).failure
}

pub fn classify_request_http(status: StatusCode, body: Option<&str>) -> FailureClass {
    analyze_request_http(status, None, body).failure
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_http_401_is_access_token_rejected() {
        assert_eq!(
            classify_request_http(StatusCode::UNAUTHORIZED, Some("Unauthorized")),
            FailureClass::AccessTokenRejected
        );
    }

    #[test]
    fn request_http_400_is_request_rejected() {
        assert_eq!(
            classify_request_http(StatusCode::BAD_REQUEST, Some("Unknown parameter")),
            FailureClass::RequestRejected
        );
    }

    #[test]
    fn request_http_rate_limit_reached_is_rate_limited() {
        assert_eq!(
            classify_request_http(
                StatusCode::TOO_MANY_REQUESTS,
                Some("Rate limit reached for gpt-5.4. Please try again later."),
            ),
            FailureClass::RateLimited
        );
    }

    #[test]
    fn request_http_structured_usage_limit_is_quota_exhausted() {
        assert_eq!(
            classify_request_http(
                StatusCode::BAD_GATEWAY,
                Some(
                    r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#
                ),
            ),
            FailureClass::QuotaExhausted
        );
    }

    #[test]
    fn request_http_structured_websocket_connection_limit_is_temporary_failure() {
        assert_eq!(
            classify_request_http(
                StatusCode::BAD_REQUEST,
                Some(
                    r#"{"error":{"type":"invalid_request_error","code":"websocket_connection_limit_reached","message":"Responses websocket connection limit reached (60 minutes). Create a new websocket connection to continue."}}"#
                ),
            ),
            FailureClass::TemporaryFailure
        );
    }

    #[test]
    fn request_http_structured_invalid_prompt_is_request_rejected_even_on_502() {
        assert_eq!(
            classify_request_http(
                StatusCode::BAD_GATEWAY,
                Some(r#"{"error":{"code":"invalid_prompt","message":"bad prompt"}}"#),
            ),
            FailureClass::RequestRejected
        );
    }

    #[test]
    fn request_http_structured_context_length_exceeded_is_request_rejected_even_on_502() {
        assert_eq!(
            classify_request_http(
                StatusCode::BAD_GATEWAY,
                Some(r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#),
            ),
            FailureClass::RequestRejected
        );
    }

    #[test]
    fn refresh_http_401_is_auth_invalid() {
        assert_eq!(
            classify_refresh_response(StatusCode::UNAUTHORIZED, "refresh_token_expired"),
            FailureClass::AuthInvalid
        );
    }

    #[test]
    fn refresh_http_rate_limit_reached_is_rate_limited() {
        assert_eq!(
            classify_refresh_response(
                StatusCode::TOO_MANY_REQUESTS,
                "Rate limit reached for refresh flow",
            ),
            FailureClass::RateLimited
        );
    }

    #[test]
    fn refresh_http_400_is_request_rejected() {
        assert_eq!(
            classify_refresh_response(StatusCode::BAD_REQUEST, "bad request"),
            FailureClass::RequestRejected
        );
    }

    #[test]
    fn refresh_http_structured_usage_limit_is_quota_exhausted() {
        assert_eq!(
            classify_refresh_response(
                StatusCode::FORBIDDEN,
                r#"{"error":{"type":"usage_limit_reached","message":"The usage limit has been reached"}}"#,
            ),
            FailureClass::QuotaExhausted
        );
    }
}
