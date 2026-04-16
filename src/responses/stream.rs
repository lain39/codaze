use super::{
    DownstreamFailureKind, SyntheticResponseFailedPayload, classify_openai_error_event,
    classify_response_failed_event, downstream_failure_kind, extract_retry_after,
    gateway_unavailable_payload, is_gateway_unavailable_status_failure,
    render_openai_stream_error_event, render_openai_stream_error_event_with_sequence,
    render_synthetic_response_failed_event, render_synthetic_response_failed_event_with_metadata,
    synthetic_response_failed_payload_from_transport,
};
use crate::app::AppState;
use crate::classifier::{FailureClass, classify_request_error};
use crate::failover::{AccountSettlement, extract_failure_reason, spawn_account_settlement};
use crate::models::ResponseShape;
use bytes::Bytes;
use futures::{FutureExt, Stream};
use serde_json::Value;
use std::collections::VecDeque;
use std::pin::Pin;
use std::task::{Context, Poll};

pub(crate) struct ManagedResponseStream {
    inner: futures::stream::BoxStream<'static, Result<Bytes, codex_client::TransportError>>,
    state: AppState,
    account_id: String,
    settled: bool,
    response_shape: ResponseShape,
    sse_state: ResponsesSseState,
    pending_output: VecDeque<Bytes>,
    close_after_pending_output: bool,
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

pub(crate) struct ResponsesSseState {
    buffer: Vec<u8>,
    line_start: bool,
    sse_mode: bool,
    last_sequence_number: Option<i64>,
    last_response_id: Option<String>,
    terminal_state: Option<ResponsesTerminalState>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SseLineStartMatch {
    Full,
    Partial,
    None,
}

impl Default for ResponsesSseState {
    fn default() -> Self {
        Self {
            buffer: Vec::new(),
            line_start: true,
            sse_mode: false,
            last_sequence_number: None,
            last_response_id: None,
            terminal_state: None,
        }
    }
}

impl ManagedResponseStream {
    pub(crate) fn new(
        state: AppState,
        account_id: String,
        inner: futures::stream::BoxStream<'static, Result<Bytes, codex_client::TransportError>>,
        response_shape: ResponseShape,
    ) -> Self {
        let shutdown = state.shutdown_token.clone().cancelled_owned().boxed();
        Self {
            inner,
            state,
            account_id,
            settled: false,
            response_shape,
            sse_state: ResponsesSseState::default(),
            pending_output: VecDeque::new(),
            close_after_pending_output: false,
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
        if let Some(bytes) = this.pending_output.pop_front() {
            return Poll::Ready(Some(Ok(bytes)));
        }
        if this.close_after_pending_output {
            return Poll::Ready(None);
        }
        if this.shutdown.as_mut().poll(cx).is_ready() {
            this.settle(AccountSettlement::Release);
            return Poll::Ready(None);
        }
        loop {
            match this.inner.as_mut().poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(bytes))) => {
                    this.sse_state.observe_bytes(
                        &bytes,
                        this.response_shape,
                        &mut this.pending_output,
                    );
                    let saw_terminal = if let Some(action) = this.sse_state.terminal_action() {
                        this.settle(action);
                        this.close_after_pending_output = true;
                        true
                    } else {
                        false
                    };
                    if let Some(chunk) = this.pending_output.pop_front() {
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                    if saw_terminal {
                        return Poll::Ready(None);
                    }
                }
                Poll::Ready(Some(Err(error))) => {
                    if this.sse_state.has_terminal_event() {
                        return Poll::Ready(None);
                    }
                    if let Some(buffered) = this.sse_state.take_terminal_passthrough_bytes() {
                        this.pending_output.push_back(buffered);
                    }
                    let failure = classify_request_error(&error);
                    let details = extract_failure_reason(&error, failure);
                    let retry_after = extract_retry_after(&error);
                    let settlement = AccountSettlement::Failure {
                        failure,
                        retry_after,
                        details,
                    };
                    let terminal_chunk = this.sse_state.synthetic_transport_error_event(
                        this.response_shape,
                        &error,
                        failure,
                    );
                    this.settle(settlement);
                    if let Some(chunk) = terminal_chunk {
                        this.pending_output.push_back(chunk);
                        this.close_after_pending_output = true;
                        if let Some(chunk) = this.pending_output.pop_front() {
                            return Poll::Ready(Some(Ok(chunk)));
                        }
                    } else {
                        let message = error.to_string();
                        return Poll::Ready(Some(Err(std::io::Error::other(message))));
                    }
                }
                Poll::Ready(None) => {
                    this.sse_state
                        .observe_eof(this.response_shape, &mut this.pending_output);
                    if let Some(action) = this.sse_state.terminal_action() {
                        this.settle(action);
                        this.close_after_pending_output = true;
                    } else {
                        let details = "stream closed before response.completed".to_string();
                        this.settle(AccountSettlement::Failure {
                            failure: FailureClass::TemporaryFailure,
                            retry_after: None,
                            details: details.clone(),
                        });

                        if let Some(buffered) = this.sse_state.take_terminal_passthrough_bytes() {
                            this.pending_output.push_back(buffered);
                        }
                        if !this.sse_state.saw_terminal_failure_event()
                            && let Some(chunk) = this.sse_state.synthetic_error_event_with_payload(
                                this.response_shape,
                                SyntheticResponseFailedPayload {
                                    code: Some("internal_server_error".to_string()),
                                    message: Some(details),
                                    error_type: None,
                                    plan_type: None,
                                    resets_at: None,
                                    resets_in_seconds: None,
                                },
                            )
                        {
                            this.pending_output.push_back(chunk);
                        }
                        this.close_after_pending_output = true;
                    }

                    if let Some(chunk) = this.pending_output.pop_front() {
                        return Poll::Ready(Some(Ok(chunk)));
                    }
                    return Poll::Ready(None);
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
            line_start: true,
            sse_mode: false,
            last_sequence_number,
            last_response_id,
            terminal_state: None,
        }
    }

    pub(crate) fn observe_bytes(
        &mut self,
        bytes: &[u8],
        response_shape: ResponseShape,
        output: &mut VecDeque<Bytes>,
    ) {
        if self.has_terminal_event() {
            return;
        }
        let mut cursor = 0;

        if !self.sse_mode && !self.buffer.is_empty() {
            cursor += self.resolve_pending_line_start(bytes, output);
            if self.sse_mode {
                self.buffer.extend_from_slice(&bytes[cursor..]);
                self.consume_buffered_events(response_shape, output);
                return;
            }
        }

        if self.sse_mode {
            self.buffer.extend_from_slice(&bytes[cursor..]);
            self.consume_buffered_events(response_shape, output);
            return;
        }

        let passthrough_start = cursor;
        while cursor < bytes.len() {
            if self.line_start {
                match classify_sse_line_start(&bytes[cursor..]) {
                    SseLineStartMatch::Full => {
                        if passthrough_start < cursor {
                            let passthrough = &bytes[passthrough_start..cursor];
                            output.push_back(Bytes::copy_from_slice(passthrough));
                            self.line_start = line_start_after_bytes(self.line_start, passthrough);
                        }
                        self.sse_mode = true;
                        self.buffer.extend_from_slice(&bytes[cursor..]);
                        self.consume_buffered_events(response_shape, output);
                        return;
                    }
                    SseLineStartMatch::Partial => {
                        if passthrough_start < cursor {
                            let passthrough = &bytes[passthrough_start..cursor];
                            output.push_back(Bytes::copy_from_slice(passthrough));
                            self.line_start = line_start_after_bytes(self.line_start, passthrough);
                        }
                        self.buffer.extend_from_slice(&bytes[cursor..]);
                        return;
                    }
                    SseLineStartMatch::None => {}
                }
            }

            self.line_start = bytes[cursor] == b'\n';
            cursor += 1;
        }

        if passthrough_start < bytes.len() {
            let passthrough = &bytes[passthrough_start..];
            output.push_back(Bytes::copy_from_slice(passthrough));
            self.line_start = line_start_after_bytes(self.line_start, passthrough);
        }
    }

    fn resolve_pending_line_start(&mut self, bytes: &[u8], output: &mut VecDeque<Bytes>) -> usize {
        let mut consumed = 0;
        while consumed < bytes.len() {
            self.buffer.push(bytes[consumed]);
            consumed += 1;
            match classify_sse_line_start(&self.buffer) {
                SseLineStartMatch::Full => {
                    self.sse_mode = true;
                    return consumed;
                }
                SseLineStartMatch::Partial => continue,
                SseLineStartMatch::None => {
                    let passthrough = std::mem::take(&mut self.buffer);
                    self.line_start = line_start_after_bytes(self.line_start, &passthrough);
                    output.push_back(Bytes::from(passthrough));
                    return consumed;
                }
            }
        }
        consumed
    }

    fn consume_buffered_events(
        &mut self,
        response_shape: ResponseShape,
        output: &mut VecDeque<Bytes>,
    ) {
        while let Some(event_end) = find_sse_event_end(&self.buffer) {
            let event_bytes = self.buffer.drain(..event_end).collect::<Vec<u8>>();
            output.push_back(self.consume_event(&event_bytes, response_shape));
            if self.has_terminal_event() {
                self.buffer.clear();
                break;
            }
        }
    }

    fn observe_eof(&mut self, response_shape: ResponseShape, output: &mut VecDeque<Bytes>) {
        if self.has_terminal_event()
            || !self.sse_mode
            || self.buffer.is_empty()
            || !self.buffer.ends_with(b"\n")
        {
            return;
        }

        let event_bytes = std::mem::take(&mut self.buffer);
        output.push_back(self.consume_event(&event_bytes, response_shape));
    }

    fn consume_event(&mut self, event_bytes: &[u8], response_shape: ResponseShape) -> Bytes {
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
            return Bytes::copy_from_slice(event_bytes);
        }

        let payload = data_lines.join("\n");
        let Ok(json) = serde_json::from_str::<Value>(&payload) else {
            return Bytes::copy_from_slice(event_bytes);
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
                let classified = classify_response_failed_event(&error);
                self.terminal_state = Some(ResponsesTerminalState::Failed {
                    failure: classified.failure,
                    retry_after: classified.retry_after,
                    details: classified.details.clone(),
                });
                if is_gateway_unavailable_status_failure(classified.status, classified.failure) {
                    let mut rewritten_json = json.clone();
                    return self.rewrite_gateway_unavailable_event(
                        response_shape,
                        kind,
                        &mut rewritten_json,
                    );
                }
            }
            "error" => {
                let classified = classify_openai_error_event(&json);
                self.terminal_state = Some(ResponsesTerminalState::Failed {
                    failure: classified.failure,
                    retry_after: classified.retry_after,
                    details: classified.details.clone(),
                });
                if is_gateway_unavailable_status_failure(classified.status, classified.failure) {
                    let mut rewritten_json = json.clone();
                    return self.rewrite_gateway_unavailable_event(
                        response_shape,
                        kind,
                        &mut rewritten_json,
                    );
                }
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

        Bytes::copy_from_slice(event_bytes)
    }

    #[cfg(test)]
    pub(crate) fn synthetic_failed_event(
        &self,
        error: &codex_client::TransportError,
    ) -> Option<Bytes> {
        self.synthetic_error_event_with_payload(
            ResponseShape::Codex,
            synthetic_response_failed_payload_from_transport(error),
        )
    }

    fn has_terminal_event(&self) -> bool {
        self.terminal_state.is_some()
    }

    fn take_terminal_passthrough_bytes(&mut self) -> Option<Bytes> {
        if self.buffer.is_empty() {
            return None;
        }
        if self.sse_mode || classify_sse_line_start(&self.buffer) == SseLineStartMatch::Partial {
            self.buffer.clear();
            return None;
        }
        Some(Bytes::from(std::mem::take(&mut self.buffer)))
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

    fn synthetic_transport_error_event(
        &self,
        response_shape: ResponseShape,
        error: &codex_client::TransportError,
        failure: FailureClass,
    ) -> Option<Bytes> {
        let payload = match error {
            codex_client::TransportError::Http { .. }
                if downstream_failure_kind(failure)
                    == DownstreamFailureKind::GatewayUnavailable =>
            {
                gateway_unavailable_payload()
            }
            _ => synthetic_response_failed_payload_from_transport(error),
        };
        self.synthetic_error_event_with_payload(response_shape, payload)
    }

    fn synthetic_error_event_with_payload(
        &self,
        response_shape: ResponseShape,
        payload: SyntheticResponseFailedPayload,
    ) -> Option<Bytes> {
        match response_shape {
            ResponseShape::Codex => render_synthetic_response_failed_event(
                self.last_response_id.as_deref(),
                self.last_sequence_number,
                payload,
            ),
            ResponseShape::OpenAi => {
                render_openai_stream_error_event(self.last_sequence_number, payload)
            }
        }
    }

    fn rewrite_gateway_unavailable_event(
        &self,
        response_shape: ResponseShape,
        kind: &str,
        json: &mut Value,
    ) -> Bytes {
        let rendered_sequence_number = json
            .get("sequence_number")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                self.last_sequence_number
                    .unwrap_or(-1)
                    .saturating_add(1)
                    .max(0)
            });
        match response_shape {
            ResponseShape::Codex if kind == "response.failed" => {
                if let Some(error) = json.pointer_mut("/response/error") {
                    *error = serde_json::to_value(gateway_unavailable_payload_to_json())
                        .unwrap_or(Value::Null);
                }
                render_sse_event_bytes(kind, json)
            }
            ResponseShape::Codex => render_synthetic_response_failed_event_with_metadata(
                self.last_response_id.as_deref(),
                rendered_sequence_number,
                gateway_unavailable_payload(),
            )
            .unwrap_or_else(|| {
                Bytes::from_static(
                    b"event: response.failed\ndata: {\"type\":\"response.failed\"}\n\n",
                )
            }),
            ResponseShape::OpenAi => render_openai_stream_error_event_with_sequence(
                rendered_sequence_number,
                gateway_unavailable_payload(),
            )
            .unwrap_or_else(|| Bytes::from_static(b"event: error\ndata: {\"type\":\"error\"}\n\n")),
        }
    }
}

fn gateway_unavailable_payload_to_json() -> serde_json::Map<String, Value> {
    let payload = gateway_unavailable_payload();
    serde_json::Map::from_iter([
        (
            "code".to_string(),
            Value::String(
                payload
                    .code
                    .unwrap_or_else(|| "server_is_overloaded".to_string()),
            ),
        ),
        (
            "message".to_string(),
            Value::String(
                payload.message.unwrap_or_else(|| {
                    "No account available right now. Try again later.".to_string()
                }),
            ),
        ),
    ])
}

fn render_sse_event_bytes(event_name: &str, json: &Value) -> Bytes {
    let encoded =
        serde_json::to_string(json).unwrap_or_else(|_| "{\"type\":\"error\"}".to_string());
    Bytes::from(format!("event: {event_name}\ndata: {encoded}\n\n"))
}

fn classify_sse_line_start(bytes: &[u8]) -> SseLineStartMatch {
    const SSE_LINE_PREFIXES: [&[u8]; 5] = [b"event:", b"data:", b"id:", b"retry:", b":"];

    if SSE_LINE_PREFIXES
        .iter()
        .any(|prefix| bytes.starts_with(prefix))
    {
        return SseLineStartMatch::Full;
    }
    if SSE_LINE_PREFIXES
        .iter()
        .any(|prefix| prefix.starts_with(bytes))
    {
        return SseLineStartMatch::Partial;
    }
    SseLineStartMatch::None
}

fn line_start_after_bytes(mut line_start: bool, bytes: &[u8]) -> bool {
    for &byte in bytes {
        line_start = byte == b'\n';
    }
    line_start
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_sse_line_start_matches_full_prefixes_at_response_start() {
        assert_eq!(
            classify_sse_line_start(b"event: response.completed"),
            SseLineStartMatch::Full
        );
        assert_eq!(
            classify_sse_line_start(b"data: {\"type\":\"response.failed\"}"),
            SseLineStartMatch::Full
        );
        assert_eq!(
            classify_sse_line_start(b": keep-alive"),
            SseLineStartMatch::Full
        );
    }

    #[test]
    fn classify_sse_line_start_matches_partial_prefixes() {
        assert_eq!(classify_sse_line_start(b"eve"), SseLineStartMatch::Partial);
        assert_eq!(classify_sse_line_start(b"da"), SseLineStartMatch::Partial);
        assert_eq!(classify_sse_line_start(b"ret"), SseLineStartMatch::Partial);
    }

    #[test]
    fn classify_sse_line_start_rejects_non_prefixes() {
        assert_eq!(classify_sse_line_start(b"hello"), SseLineStartMatch::None);
        assert_eq!(classify_sse_line_start(b" event:"), SseLineStartMatch::None);
    }

    #[test]
    fn observe_bytes_detects_sse_at_response_start() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\"}}\n\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        let chunk = output.pop_front().expect("chunk");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains("event: response.completed"));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Success)
        ));
    }

    #[test]
    fn observe_bytes_detects_sse_after_newline_and_preserves_prior_passthrough() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"hello\nevent: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\"}}\n\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 2);
        assert_eq!(
            output.pop_front().expect("passthrough"),
            Bytes::from_static(b"hello\n")
        );
        let chunk = output.pop_front().expect("event");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains("event: response.completed"));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Success)
        ));
    }

    #[test]
    fn observe_bytes_does_not_match_embedded_event_text_mid_line() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"hello event: response.completed",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        assert_eq!(
            output.pop_front().expect("passthrough"),
            Bytes::from_static(b"hello event: response.completed")
        );
        assert!(state.terminal_action().is_none());
    }

    #[test]
    fn observe_bytes_detects_sse_with_crlf_event_boundaries() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"event: response.completed\r\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\"}}\r\n\r\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        let chunk = output.pop_front().expect("chunk");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains("event: response.completed"));
        assert!(text.contains("\"type\":\"response.completed\""));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Success)
        ));
    }

    #[test]
    fn observe_bytes_joins_multiple_data_lines_in_one_event() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"event: response.incomplete\ndata: {\"type\":\"response.incomplete\",\ndata: \"response\":{\"incomplete_details\":{\"reason\":\"max_output_tokens\"}}}\n\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        let chunk = output.pop_front().expect("chunk");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains("event: response.incomplete"));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Failure {
                failure: FailureClass::RequestRejected,
                retry_after: None,
                ref details,
            }) if details == "Incomplete response returned, reason: max_output_tokens"
        ));
    }

    #[test]
    fn observe_bytes_joins_multiple_data_lines_with_crlf_in_one_event() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"data: {\"type\":\"response.failed\",\r\ndata: \"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached\"}}}\r\n\r\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        let chunk = output.pop_front().expect("chunk");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains("\"type\":\"response.failed\""));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Failure {
                failure: FailureClass::RateLimited,
                ..
            })
        ));
    }

    #[test]
    fn observe_bytes_preserves_id_retry_and_comment_lines_inside_event() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b": keep-alive\nevent: response.completed\nid: evt_123\nretry: 5000\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\"}}\n\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        let chunk = output.pop_front().expect("chunk");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains(": keep-alive"));
        assert!(text.contains("id: evt_123"));
        assert!(text.contains("retry: 5000"));
        assert!(text.contains("event: response.completed"));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Success)
        ));
    }

    #[test]
    fn observe_bytes_preserves_passthrough_prefix_before_sse_in_same_chunk() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(
            b"hello\ndata: {\"type\":\"response.failed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_rate_limit\",\"error\":{\"code\":\"rate_limit_exceeded\",\"message\":\"Rate limit reached\"}}}\n\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 2);
        assert_eq!(
            output.pop_front().expect("passthrough"),
            Bytes::from_static(b"hello\n")
        );
        let chunk = output.pop_front().expect("event");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains("\"type\":\"response.failed\""));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Failure {
                failure: FailureClass::RateLimited,
                ..
            })
        ));
    }

    #[test]
    fn observe_bytes_handles_comment_chunk_followed_by_event_chunk() {
        let mut state = ResponsesSseState::default();
        let mut output = VecDeque::new();

        state.observe_bytes(b": keep-alive\n", ResponseShape::Codex, &mut output);

        assert_eq!(output.len(), 0);
        assert!(state.terminal_action().is_none());

        state.observe_bytes(
            b"event: response.completed\ndata: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp_done\"}}\n\n",
            ResponseShape::Codex,
            &mut output,
        );

        assert_eq!(output.len(), 1);
        let chunk = output.pop_front().expect("chunk");
        let text = String::from_utf8(chunk.to_vec()).expect("utf8");
        assert!(text.contains(": keep-alive"));
        assert!(text.contains("event: response.completed"));
        assert!(matches!(
            state.terminal_action(),
            Some(AccountSettlement::Success)
        ));
    }
}
