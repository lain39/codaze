use super::{
    SyntheticResponseFailedPayload, classify_response_failed_event, extract_retry_after,
    render_synthetic_response_failed_event, synthetic_response_failed_payload_from_transport,
};
use crate::app::AppState;
use crate::classifier::{FailureClass, classify_request_error};
use crate::failover::{AccountSettlement, extract_failure_reason, spawn_account_settlement};
use bytes::Bytes;
use futures::{FutureExt, Stream};
use serde_json::Value;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ManagedResponseStream {
    inner: futures::stream::BoxStream<'static, Result<Bytes, codex_client::TransportError>>,
    state: AppState,
    account_id: String,
    settled: bool,
    sse_state: ResponsesSseState,
    finished_after_terminal: bool,
    shutdown: futures::future::BoxFuture<'static, ()>,
}

#[derive(Debug)]
enum ResponsesTerminalState {
    Completed,
    Failed {
        failure: FailureClass,
        retry_after: Option<std::time::Duration>,
        details: String,
    },
}

#[derive(Default)]
pub(crate) struct ResponsesSseState {
    buffer: Vec<u8>,
    last_sequence_number: Option<i64>,
    last_response_id: Option<String>,
    terminal_state: Option<ResponsesTerminalState>,
}

impl ManagedResponseStream {
    pub(crate) fn new(
        state: AppState,
        account_id: String,
        inner: futures::stream::BoxStream<'static, Result<Bytes, codex_client::TransportError>>,
    ) -> Self {
        let shutdown = state.shutdown_token.clone().cancelled_owned().boxed();
        Self {
            inner,
            state,
            account_id,
            settled: false,
            sse_state: ResponsesSseState::default(),
            finished_after_terminal: false,
            shutdown,
        }
    }

    fn settle(&mut self, settlement: AccountSettlement) {
        if self.settled {
            return;
        }
        self.settled = true;
        spawn_account_settlement(self.state.clone(), self.account_id.clone(), settlement);
    }
}

impl Stream for ManagedResponseStream {
    type Item = Result<Bytes, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        if this.finished_after_terminal {
            return Poll::Ready(None);
        }
        if this.shutdown.as_mut().poll(cx).is_ready() {
            this.settle(AccountSettlement::Release);
            return Poll::Ready(None);
        }
        match this.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(bytes))) => {
                this.sse_state.observe_bytes(&bytes);
                if let Some(action) = this.sse_state.terminal_action() {
                    this.settle(action);
                }
                Poll::Ready(Some(Ok(bytes)))
            }
            Poll::Ready(Some(Err(error))) => {
                if this.sse_state.has_terminal_event() {
                    return Poll::Ready(None);
                }
                let failure = classify_request_error(&error);
                let details = extract_failure_reason(&error, failure);
                let retry_after = extract_retry_after(&error);
                let settlement = AccountSettlement::Failure {
                    failure,
                    retry_after,
                    details,
                };
                let terminal_chunk = this.sse_state.synthetic_failed_event(&error);
                this.settle(settlement);
                if let Some(chunk) = terminal_chunk {
                    this.finished_after_terminal = true;
                    Poll::Ready(Some(Ok(chunk)))
                } else {
                    let message = error.to_string();
                    Poll::Ready(Some(Err(std::io::Error::other(message))))
                }
            }
            Poll::Ready(None) => {
                if let Some(action) = this.sse_state.terminal_action() {
                    this.settle(action);
                    return Poll::Ready(None);
                }

                if this.sse_state.completed_successfully() {
                    this.settle(AccountSettlement::Success);
                    return Poll::Ready(None);
                }

                let details = "stream closed before response.completed".to_string();
                this.settle(AccountSettlement::Failure {
                    failure: FailureClass::TemporaryFailure,
                    retry_after: None,
                    details: details.clone(),
                });

                if this.sse_state.saw_terminal_failure_event() {
                    Poll::Ready(None)
                } else if let Some(chunk) = this.sse_state.synthetic_failed_event_with_payload(
                    SyntheticResponseFailedPayload {
                        code: Some("internal_server_error".to_string()),
                        message: Some(details),
                        error_type: None,
                        plan_type: None,
                        resets_at: None,
                        resets_in_seconds: None,
                    },
                ) {
                    this.finished_after_terminal = true;
                    Poll::Ready(Some(Ok(chunk)))
                } else {
                    Poll::Ready(None)
                }
            }
        }
    }
}

impl Drop for ManagedResponseStream {
    fn drop(&mut self) {
        if !self.settled {
            if let Some(action) = self.sse_state.terminal_action() {
                self.settle(action);
            } else {
                self.settle(AccountSettlement::Release);
            }
        }
    }
}

impl ResponsesSseState {
    #[cfg(test)]
    pub(crate) fn with_checkpoint(
        last_sequence_number: Option<i64>,
        last_response_id: Option<String>,
    ) -> Self {
        Self {
            buffer: Vec::new(),
            last_sequence_number,
            last_response_id,
            terminal_state: None,
        }
    }

    pub(crate) fn observe_bytes(&mut self, bytes: &[u8]) {
        self.buffer.extend_from_slice(bytes);
        while let Some(event_end) = find_sse_event_end(&self.buffer) {
            let event_bytes = self.buffer.drain(..event_end).collect::<Vec<u8>>();
            self.consume_event(&event_bytes);
        }
    }

    fn consume_event(&mut self, event_bytes: &[u8]) {
        let text = String::from_utf8_lossy(event_bytes);
        let mut data_lines = Vec::new();
        let mut event_name: Option<&str> = None;

        for raw_line in text.lines() {
            let line = raw_line.trim_end_matches('\r');
            if let Some(name) = line.strip_prefix("event:") {
                event_name = Some(name.trim_start());
            }
            if let Some(data) = line.strip_prefix("data:") {
                data_lines.push(data.trim_start());
            }
        }

        if data_lines.is_empty() {
            return;
        }

        let payload = data_lines.join("\n");
        let Ok(json) = serde_json::from_str::<Value>(&payload) else {
            return;
        };

        if let Some(sequence_number) = json.get("sequence_number").and_then(Value::as_i64) {
            self.last_sequence_number = Some(sequence_number);
        }

        if let Some(response_id) = json
            .get("response")
            .and_then(|response| response.get("id"))
            .and_then(Value::as_str)
        {
            self.last_response_id = Some(response_id.to_string());
        }

        let kind = event_name
            .or_else(|| json.get("type").and_then(Value::as_str))
            .unwrap_or_default();
        match kind {
            "response.completed" => {
                self.terminal_state = Some(ResponsesTerminalState::Completed);
            }
            "response.failed" => {
                let error = json
                    .get("response")
                    .and_then(|response| response.get("error"))
                    .cloned()
                    .unwrap_or(Value::Null);
                let (failure, retry_after, details) = classify_response_failed_event(&error);
                self.terminal_state = Some(ResponsesTerminalState::Failed {
                    failure,
                    retry_after,
                    details,
                });
            }
            "response.incomplete" => {
                let reason = json
                    .get("response")
                    .and_then(|response| response.get("incomplete_details"))
                    .and_then(|details| details.get("reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                self.terminal_state = Some(ResponsesTerminalState::Failed {
                    failure: FailureClass::RequestRejected,
                    retry_after: None,
                    details: format!("Incomplete response returned, reason: {reason}"),
                });
            }
            _ => {}
        }
    }

    pub(crate) fn synthetic_failed_event(
        &self,
        error: &codex_client::TransportError,
    ) -> Option<Bytes> {
        render_synthetic_response_failed_event(
            self.last_response_id.as_deref(),
            self.last_sequence_number,
            synthetic_response_failed_payload_from_transport(error),
        )
    }

    pub(crate) fn synthetic_failed_event_with_payload(
        &self,
        payload: SyntheticResponseFailedPayload,
    ) -> Option<Bytes> {
        render_synthetic_response_failed_event(
            self.last_response_id.as_deref(),
            self.last_sequence_number,
            payload,
        )
    }

    fn completed_successfully(&self) -> bool {
        matches!(self.terminal_state, Some(ResponsesTerminalState::Completed))
    }

    fn has_terminal_event(&self) -> bool {
        self.terminal_state.is_some()
    }

    fn saw_terminal_failure_event(&self) -> bool {
        matches!(
            self.terminal_state,
            Some(ResponsesTerminalState::Failed { .. })
        )
    }

    fn terminal_action(&self) -> Option<AccountSettlement> {
        match &self.terminal_state {
            Some(ResponsesTerminalState::Completed) => Some(AccountSettlement::Success),
            Some(ResponsesTerminalState::Failed {
                failure,
                retry_after,
                details,
            }) => Some(AccountSettlement::Failure {
                failure: *failure,
                retry_after: *retry_after,
                details: details.clone(),
            }),
            None => None,
        }
    }
}

fn find_sse_event_end(buffer: &[u8]) -> Option<usize> {
    for index in 0..buffer.len().saturating_sub(1) {
        if buffer[index] == b'\n' && buffer[index + 1] == b'\n' {
            return Some(index + 2);
        }
        if index + 3 < buffer.len()
            && buffer[index] == b'\r'
            && buffer[index + 1] == b'\n'
            && buffer[index + 2] == b'\r'
            && buffer[index + 3] == b'\n'
        {
            return Some(index + 4);
        }
    }
    None
}
