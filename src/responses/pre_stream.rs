#[cfg(test)]
use crate::failover::FailoverFailure;
#[cfg(test)]
use crate::gateway_errors::{FailureRenderMode, render_failover_failure};
#[cfg(test)]
use crate::models::ResponseShape;
#[cfg(test)]
use axum::response::Response;

#[cfg(test)]
pub(crate) fn responses_pre_stream_failure_response(
    error: &FailoverFailure,
    response_shape: ResponseShape,
) -> Response {
    render_failover_failure(error, response_shape, FailureRenderMode::ResponsesPreStream)
}
