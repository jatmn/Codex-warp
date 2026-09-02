use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::OnceLock;

use async_stream::stream;
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;
use serde::de::IgnoredAny;
use serde_json::Value;
use serde_json::json;

use crate::config::ContinueGuardConfig;
use crate::config::ContinueGuardMode;
use crate::config::ToolPolicyConfig;
use crate::debug_log::DebugLog;
use crate::debug_log::text_fingerprint;
use crate::ids::generated_id;
use crate::namespace_helpers::NamespaceHelpers;
use crate::namespace_helpers::RewrittenCall;
use crate::namespace_helpers::is_custom_tool_call_type;
use crate::namespace_helpers::is_function_call_type;
use crate::provider::complete_session_model_update;
use crate::state::AppState;
use crate::store::UsageRecorder;
use crate::tool_markup::Sanitizer;
use crate::tool_policy::apply_tool_policy_to_function_call;

const REASONING_DISPLAY_HEADER: &str = "**Reasoning**\n\n";

/// Coalesce provider-specific token or line-sized reasoning deltas into the
/// small display blocks used by Codex clients for native reasoning summaries.
/// A paragraph boundary still flushes immediately so structured reasoning is
/// not held behind an arbitrary character count.
const REASONING_DELTA_FLUSH_CHARS: usize = 160;
const REASONING_DELTA_FLUSH_PARAGRAPH: &str = "\n\n";

fn reasoning_should_flush(pending: &str) -> bool {
    let text_only = pending
        .strip_prefix(REASONING_DISPLAY_HEADER)
        .unwrap_or(pending);
    !text_only.is_empty()
        && (text_only.chars().count() >= REASONING_DELTA_FLUSH_CHARS
            || text_only.contains(REASONING_DELTA_FLUSH_PARAGRAPH))
}

/// Buffers native `response.reasoning_summary_text.delta` events so tiny
/// provider fragments are coalesced into the same small display blocks used
/// for chat-completions reasoning.
#[derive(Default)]
struct NativeReasoningBuffer {
    pending: String,
    active: Option<NativeReasoningIdentity>,
}

struct NativeReasoningIdentity {
    template: Value,
    item_id: String,
    summary_index: u64,
}

impl NativeReasoningBuffer {
    fn append(&mut self, delta: &NativeReasoningSummaryDelta) -> Option<String> {
        // Identity is the Responses (item_id, summary_index) pair. A change
        // means the previous buffered summary is complete and must flush first.
        let identity_changed = self.active.as_ref().is_some_and(|active| {
            active.item_id != delta.item_id || active.summary_index != delta.summary_index
        });
        let flushed = if identity_changed {
            self.take_all()
        } else {
            None
        };
        if self.active.is_none() {
            self.pending.clear();
            self.active = Some(NativeReasoningIdentity {
                template: delta.template.clone(),
                item_id: delta.item_id.clone(),
                summary_index: delta.summary_index,
            });
        }
        self.pending.push_str(&delta.text);
        flushed
    }

    fn take_flush(&mut self) -> Option<String> {
        reasoning_should_flush(&self.pending)
            .then(|| self.take_all())
            .flatten()
    }

    fn take_all(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            self.active = None;
            return None;
        }
        let mut value = self.active.take()?.template;
        if let Some(delta) = value.get_mut("delta") {
            *delta = Value::String(self.pending.clone());
        }
        self.pending.clear();
        Some(sse("response.reasoning_summary_text.delta", value))
    }
}

struct NativeReasoningSummaryDelta {
    item_id: String,
    summary_index: u64,
    text: String,
    template: Value,
}

/// A native SSE frame can be a reasoning delta, a meaningful Responses event,
/// or transport-only framing such as a comment heartbeat. Only meaningful
/// Responses events establish a boundary for buffered reasoning.
enum NativeReasoningFrame {
    Reasoning(NativeReasoningSummaryDelta),
    Other,
    DataLess,
}

fn classify_native_reasoning_frame(frame: &str) -> NativeReasoningFrame {
    let Some(data) = sse_data(frame) else {
        return NativeReasoningFrame::DataLess;
    };
    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
        return NativeReasoningFrame::Other;
    };
    if value.get("type").and_then(Value::as_str) != Some("response.reasoning_summary_text.delta") {
        return NativeReasoningFrame::Other;
    }
    let item_id = value
        .get("item_id")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let summary_index = value
        .get("summary_index")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let Some(text) = value
        .get("delta")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return NativeReasoningFrame::Other;
    };
    // Keep a template of the original event with an empty delta so later flushes
    // preserve provider fields without panicking on unexpected shapes.
    let Some(object) = value.as_object_mut() else {
        return NativeReasoningFrame::Other;
    };
    object.insert("delta".to_string(), Value::String(String::new()));
    NativeReasoningFrame::Reasoning(NativeReasoningSummaryDelta {
        item_id,
        summary_index,
        text,
        template: value,
    })
}

/// Maximum number of entries allowed in the continue-guard budget map to prevent
/// unbounded memory growth during long-running proxy sessions.
const CONTINUE_GUARD_BUDGET_MAX_ENTRIES: usize = 10_000;

/// Maximum number of bytes allowed in the SSE frame buffer before treating the
/// upstream as misbehaving and returning an error.
const SSE_FRAME_BUFFER_MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_REPAIRED_CONCATENATED_TOOL_CALLS: usize = 64;
const MAX_REPAIRED_TOOL_CALL_ARGUMENT_BYTES: usize = 1024 * 1024;
const SSE_FRAME_BUFFER_EXCEEDED_MESSAGE: &str = "upstream SSE frame buffer exceeded maximum size";

// Stream conversion carries request context rather than a new struct.
#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))] // the route integration slice wires this adapter
pub(crate) fn chat_stream_to_responses(
    upstream: reqwest::Response,
    response_id: String,
    custom_tool_names: BTreeSet<String>,
    namespace_helpers: NamespaceHelpers,
    tool_policy: ToolPolicyConfig,
    debug_log: DebugLog,
    request_log_id: String,
    continue_guard: ContinueGuardState,
    usage_recorder: Option<UsageRecorder>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    chat_stream_to_responses_with_session_model(
        upstream,
        response_id,
        custom_tool_names,
        namespace_helpers,
        tool_policy,
        debug_log,
        request_log_id,
        continue_guard,
        usage_recorder,
        false,
        false,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn chat_stream_to_responses_with_session_model(
    upstream: reqwest::Response,
    response_id: String,
    custom_tool_names: BTreeSet<String>,
    namespace_helpers: NamespaceHelpers,
    tool_policy: ToolPolicyConfig,
    debug_log: DebugLog,
    request_log_id: String,
    continue_guard: ContinueGuardState,
    usage_recorder: Option<UsageRecorder>,
    suppress_duplicate_tool_markup: bool,
    split_concatenated_tool_call_arguments: bool,
    session_model: Option<(AppState, crate::state::SessionModelUpdate)>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    stream! {
        let created_event = sse("response.created", json!({
            "type": "response.created",
            "response": {"id": response_id, "object": "response", "status": "in_progress"}
        }));
        log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &created_event);
        yield Ok(Bytes::from(created_event));

        let mut state = ChatAccum::with_tool_markup_suppression(suppress_duplicate_tool_markup);
        state.split_concatenated_tool_call_arguments = split_concatenated_tool_call_arguments;
        let mut pending = Vec::new();
        let mut bytes = upstream.bytes_stream();
        let mut completed = false;
        let mut response_observed = false;
        let usage_recorder = usage_recorder;

        'upstream: while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    for event in state.failure_events() {
                        log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                        yield Ok(Bytes::from(event));
                    }
                    yield Ok(Bytes::from(chat_failed_event(&response_id, err.to_string())));
                    return;
                }
            };
            pending.extend_from_slice(&chunk);
            if pending.len() > SSE_FRAME_BUFFER_MAX_BYTES {
                for event in state.failure_events() {
                    log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                    yield Ok(Bytes::from(event));
                }
                yield Ok(Bytes::from(chat_failed_event(&response_id, "upstream SSE frame buffer exceeded maximum size")));
                return;
            }

            while let Some((frame_end, delimiter_len)) = next_sse_frame_bytes(&pending) {
                let frame = pending[..frame_end].to_vec();
                pending.drain(..frame_end + delimiter_len);
                let Ok(frame) = String::from_utf8(frame) else {
                    for event in state.failure_events() {
                        log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                        yield Ok(Bytes::from(event));
                    }
                    yield Ok(Bytes::from(chat_failed_event(&response_id, "upstream SSE frame was not valid UTF-8")));
                    return;
                };
                debug_log.log_stream_frame(json!({
                    "event": "upstream_stream_frame",
                    "id": request_log_id,
                    "backend": "open_ai_chat"
                }), &frame);
                let Some(data) = sse_data(&frame) else {
                    continue;
                };
                if data == "[DONE]" {
                    completed = true;
                    break 'upstream;
                }
                let value = match serde_json::from_str::<Value>(&data) {
                    Ok(value) => value,
                    Err(_) => {
                        for event in state.failure_events() {
                            log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                            yield Ok(Bytes::from(event));
                        }
                        yield Ok(Bytes::from(chat_failed_event(
                            &response_id,
                            "upstream chat stream contained invalid JSON",
                        )));
                        return;
                    }
                };
                let payload = chat_completion_payload(&value);
                if let Some(message) = upstream_error_message(payload) {
                    for event in state.failure_events() {
                        log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                        yield Ok(Bytes::from(event));
                    }
                    yield Ok(Bytes::from(chat_failed_event(&response_id, message)));
                    return;
                }
                response_observed |= payload
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                    .and_then(Value::as_object)
                    .is_some_and(|choice| {
                        choice.get("delta").and_then(Value::as_object).is_some()
                            || choice.get("finish_reason").and_then(Value::as_str).is_some()
                    });
                if let Some(usage) = payload.get("usage")
                    && !usage.is_null()
                {
                    let normalized = chat_usage_to_responses_usage(Some(usage));
                    debug_log.log(json!({
                        "event": "upstream_response",
                        "id": request_log_id,
                        "status": 200,
                        "success": true,
                        "usage": usage,
                        "normalized_usage": normalized.clone()
                    }));
                }
                let events = state.apply_chat_chunk(payload);
                if let Some(summary) = chat_stream_debug_summary(payload, &events) {
                    debug_log.log(json!({
                        "event": "upstream_stream_delta",
                        "id": request_log_id,
                        "backend": "open_ai_chat",
                        "summary": summary
                    }));
                }
                for event in events {
                    log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                    yield Ok(Bytes::from(event));
                }
            }
        }

        if completed {
            if !response_observed {
                debug_log.log(json!({
                    "event": "upstream_stream_complete",
                    "id": request_log_id,
                    "backend": "open_ai_chat",
                    "completion": "truncated_eof"
                }));
                let failed = chat_failed_event(&response_id, "upstream chat stream ended before [DONE]");
                for event in state.failure_events() {
                    log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                    yield Ok(Bytes::from(event));
                }
                log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &failed);
                yield Ok(Bytes::from(failed));
                return;
            }
            debug_log.log(json!({
                "event": "upstream_stream_complete",
                "id": request_log_id,
                "backend": "open_ai_chat",
                "completion": "upstream_done"
            }));
        } else if state.has_semantic_terminal_finish_reason() {
            debug_log.log(json!({
                "event": "upstream_stream_complete",
                "id": request_log_id,
                "backend": "open_ai_chat",
                "completion": "semantic_terminal_eof"
            }));
        } else {
            debug_log.log(json!({
                "event": "upstream_stream_complete",
                "id": request_log_id,
                "backend": "open_ai_chat",
                "completion": "truncated_eof"
            }));
            let failed = chat_failed_event(&response_id, "upstream chat stream ended before [DONE]");
            for event in state.failure_events() {
                log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
                yield Ok(Bytes::from(event));
            }
            log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &failed);
            yield Ok(Bytes::from(failed));
            return;
        }

        if let Some(recorder) = &usage_recorder {
            recorder.record_completed(state.usage.as_ref());
        }
        if let Some((session_state, update)) = session_model {
            complete_session_model_update(&session_state, &update).await;
        }

        for event in state.finish(
            &response_id,
            &custom_tool_names,
            &namespace_helpers,
            &tool_policy,
            Some((&debug_log, &request_log_id, &continue_guard)),
        ) {
            log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &event);
            yield Ok(Bytes::from(event));
        }
    }
}

/// Extract an OpenAI-compatible semantic error body. Gateways sometimes return
/// these inside a successful HTTP response (and chat stream frames may wrap the
/// body in `data`), so HTTP status alone cannot establish completion.
pub(crate) fn upstream_error_message(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    match error {
        Value::Null => None,
        Value::String(message) if !message.is_empty() => Some(message.clone()),
        Value::Object(_) => error
            .get("message")
            .and_then(Value::as_str)
            .filter(|message| !message.is_empty())
            .map(ToOwned::to_owned)
            .or_else(|| Some("upstream chat stream returned an error".to_string())),
        _ => Some("upstream chat stream returned an error".to_string()),
    }
}

/// A native Responses response payload is well-formed for completion accounting
/// only when it has a recognizable shape (`id` / `object` / `output`) and carries
/// no provider-declared error. Shared by the streaming (`native_sse_terminal`)
/// and buffered (`response_reports_completed_or_incomplete`) completion
/// predicates so a malformed incomplete response (for example `response: {}`) is
/// rejected identically on both paths.
pub(crate) fn native_response_is_well_formed_response(response: &Value) -> bool {
    response.as_object().is_some()
        && (response
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| !id.is_empty())
            || response.get("object").and_then(Value::as_str) == Some("response")
            || response.get("output").and_then(Value::as_array).is_some())
        && upstream_error_message(response).is_none()
}

// Native SSE conversion carries the same request context.
#[allow(clippy::too_many_arguments)]
#[cfg(test)]
pub(crate) fn native_stream_to_responses(
    upstream: reqwest::Response,
    custom_tool_names: BTreeSet<String>,
    namespace_helpers: NamespaceHelpers,
    tool_policy: ToolPolicyConfig,
    debug_log: DebugLog,
    request_log_id: String,
    status: u16,
    usage_recorder: Option<UsageRecorder>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    native_stream_to_responses_with_session_model(
        upstream,
        custom_tool_names,
        namespace_helpers,
        tool_policy,
        debug_log,
        request_log_id,
        status,
        usage_recorder,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn native_stream_to_responses_with_session_model(
    upstream: reqwest::Response,
    custom_tool_names: BTreeSet<String>,
    namespace_helpers: NamespaceHelpers,
    tool_policy: ToolPolicyConfig,
    debug_log: DebugLog,
    request_log_id: String,
    status: u16,
    usage_recorder: Option<UsageRecorder>,
    session_model: Option<(AppState, crate::state::SessionModelUpdate)>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    stream! {
        let mut pending = Vec::new();
        let mut bytes = upstream.bytes_stream();
        let mut pending_usage: Option<Value> = None;
        let mut terminal_received = false;
        let mut usage_recorder = usage_recorder;
        let mut response_id = None;
        let mut native_reasoning_buffer = NativeReasoningBuffer::default();

        'upstream: while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    if let Some(flushed) = native_reasoning_buffer.take_all() {
                        log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                        yield Ok(Bytes::from(flushed));
                    }
                    yield Err(std::io::Error::other(err));
                    return;
                }
            };
            pending.extend_from_slice(&chunk);
            if pending.len() > SSE_FRAME_BUFFER_MAX_BYTES {
                if let Some(flushed) = native_reasoning_buffer.take_all() {
                    log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                    yield Ok(Bytes::from(flushed));
                }
                yield Err(std::io::Error::other(SSE_FRAME_BUFFER_EXCEEDED_MESSAGE));
                return;
            }
            while let Some((frame_end, delimiter_len)) = next_sse_frame_bytes(&pending) {
                let frame = pending[..frame_end].to_vec();
                pending.drain(..frame_end + delimiter_len);
                let frame = match String::from_utf8(frame) {
                    Ok(frame) => frame,
                    Err(err) => {
                        if let Some(flushed) = native_reasoning_buffer.take_all() {
                            log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                            yield Ok(Bytes::from(flushed));
                        }
                        yield Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
                        return;
                    }
                };
                response_id = native_sse_response_id(&frame).or(response_id);
                if let Some(message) = native_sse_error_message(&frame) {
                    if let Some(flushed) = native_reasoning_buffer.take_all() {
                        log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                        yield Ok(Bytes::from(flushed));
                    }
                    let failed = native_failed_event(response_id.as_deref(), message);
                    log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &failed);
                    yield Ok(Bytes::from(failed));
                    return;
                }
                log_native_usage_from_sse_frame(
                    &frame,
                    &debug_log,
                    &request_log_id,
                    status,
                    &mut pending_usage,
                );
                let terminal = native_sse_terminal(&frame);
                terminal_received |= terminal.is_some();
                // A completed or incomplete response both consumed tokens and
                // carry a `usage` block; record analytics for either so a
                // truncated (incomplete) response is not reported as 0 usage.
                if (terminal == Some(NativeSseTerminal::Completed)
                    || terminal == Some(NativeSseTerminal::Incomplete))
                    && let Some(recorder) = usage_recorder.take()
                {
                    recorder.record_completed(pending_usage.as_ref());
                }
                if terminal == Some(NativeSseTerminal::Completed)
                    && let Some((session_state, update)) = &session_model
                {
                    complete_session_model_update(session_state, update).await;
                }
                debug_log.log_stream_frame(json!({
                    "event": "upstream_stream_frame",
                    "id": request_log_id,
                    "backend": "responses",
                    "status": status
                }), &frame);
                let morphed = morph_native_sse_frame(
                    &frame,
                    &custom_tool_names,
                    &namespace_helpers,
                    &tool_policy,
                );
                match classify_native_reasoning_frame(&morphed) {
                    NativeReasoningFrame::Reasoning(reasoning) => {
                        if let Some(flushed) = native_reasoning_buffer.append(&reasoning) {
                            log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                            yield Ok(Bytes::from(flushed));
                        }
                        if let Some(flushed) = native_reasoning_buffer.take_flush() {
                            log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                            yield Ok(Bytes::from(flushed));
                        }
                    }
                    NativeReasoningFrame::Other => {
                        if let Some(flushed) = native_reasoning_buffer.take_all() {
                            log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                            yield Ok(Bytes::from(flushed));
                        }
                        log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &morphed);
                        yield Ok(Bytes::from(morphed));
                    }
                    NativeReasoningFrame::DataLess => {
                        log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &morphed);
                        yield Ok(Bytes::from(morphed));
                    }
                }
                if terminal.is_some() {
                    break 'upstream;
                }
            }
        }

        if !terminal_received {
            if let Some(flushed) = native_reasoning_buffer.take_all() {
                log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &flushed);
                yield Ok(Bytes::from(flushed));
            }
            let message = if pending.is_empty() {
                "upstream Responses stream ended before a terminal response event"
            } else {
                "upstream Responses stream ended with an incomplete SSE frame"
            };
            let failed = native_failed_event(response_id.as_deref(), message);
            log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &failed);
            yield Ok(Bytes::from(failed));
        }
    }
}

fn native_sse_error_message(frame: &str) -> Option<String> {
    let data = sse_data(frame)?;
    let value = serde_json::from_str::<Value>(&data).ok()?;
    upstream_error_message(&value).or_else(|| {
        (value.get("type").and_then(Value::as_str) == Some("error"))
            .then(|| value.get("message").and_then(Value::as_str))
            .flatten()
            .filter(|message| !message.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn native_sse_response_id(frame: &str) -> Option<String> {
    let data = sse_data(frame)?;
    let value = serde_json::from_str::<Value>(&data).ok()?;
    value
        .get("response")?
        .get("id")?
        .as_str()
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NativeSseTerminal {
    Completed,
    Incomplete,
    NonSuccess,
}

/// Classify response-level terminal events independently from success. Native
/// streams can finish successfully, fail, be cancelled, or be incomplete; all
/// of those outcomes make EOF expected, but a response that produced tokens
/// (completed or incomplete) still records successful usage analytics.
///
/// `response.incomplete` is a distinct terminal *event type* (not a
/// `response.completed` whose `status` is `incomplete`, although that shape is
/// also handled below). Both forms still carry a `usage` block — for example a
/// max-output truncation — so treating either as `NonSuccess` and skipping
/// analytics would drop every token that led to the truncation, which is the
/// same "used but shows 0" gap as a missing stream usage chunk.
///
/// Both incomplete forms are only treated as a successful analytics terminal
/// when the response payload is well-formed (a recognizable shape: `id` /
/// `object` / `output`) and carries no provider error envelope. A malformed or
/// error-shaped incomplete event (for example `data: {"type":"response.
/// incomplete"}`, `response: {}`, or one wrapping a provider error) must not
/// reach `record_completed`, or it would inflate the prompt/session counters
/// and record a provider failure as successful usage. Both streaming arms use
/// the same `native_response_is_well_formed_response` predicate as the buffered
/// path (`response_reports_completed_or_incomplete`) so malformed incomplete
/// responses are rejected identically.
fn native_sse_terminal(frame: &str) -> Option<NativeSseTerminal> {
    let data = sse_data(frame)?;
    let value = serde_json::from_str::<Value>(&data).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "response.completed" => {
            let response_value = value.get("response")?;
            let response = response_value.as_object()?;
            match response.get("status").and_then(Value::as_str) {
                None | Some("completed") => Some(NativeSseTerminal::Completed),
                Some("incomplete") => {
                    // A truncated response still carries a usage block and must
                    // record analytics, but only when the response payload is
                    // well-formed (recognizable shape, no provider error). This
                    // mirrors the buffered-path guard so a malformed incomplete
                    // response is rejected identically on both paths.
                    if native_response_is_well_formed_response(response_value) {
                        Some(NativeSseTerminal::Incomplete)
                    } else {
                        None
                    }
                }
                _ => Some(NativeSseTerminal::NonSuccess),
            }
        }
        "response.failed" | "response.cancelled" => Some(NativeSseTerminal::NonSuccess),
        // `response.incomplete` is its own terminal event type (distinct from a
        // `response.completed` whose `status` is `incomplete`). Both still carry
        // a `usage` block and must record analytics rather than be treated as
        // an unrecognized frame. Per the OpenAI Responses API, `response.
        // incomplete` is a terminal event emitted as the final frame, so the
        // proxy relies on that contract: it records usage and ends the stream
        // on the first such terminal.
        "response.incomplete" => {
            // Require a well-formed response payload (recognizable shape, no
            // provider error envelope) before counting this as a successful
            // analytics terminal; otherwise record_completed would inflate
            // prompt/session counters or record a failure as usage. Uses the
            // same predicate as the buffered path.
            let response = value.get("response");
            if response.is_some_and(native_response_is_well_formed_response) {
                Some(NativeSseTerminal::Incomplete)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn chat_failed_event(response_id: &str, message: impl Into<String>) -> String {
    sse(
        "response.failed",
        json!({
            "type": "response.failed",
            "response": {"id": response_id, "object": "response", "status": "failed", "error": {"message": message.into()}}
        }),
    )
}

fn native_failed_event(response_id: Option<&str>, message: impl Into<String>) -> String {
    let message = message.into();
    match response_id {
        Some(response_id) => sse(
            "response.failed",
            json!({
                "type": "response.failed",
                "response": {"id": response_id, "object": "response", "status": "failed", "error": {"message": message}}
            }),
        ),
        None => sse("error", json!({"type": "error", "message": message})),
    }
}

pub(crate) fn response_usage_from_bytes(bytes: &Bytes) -> Value {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| native_response_usage(&value).cloned())
        .unwrap_or(Value::Null)
}

/// Native Responses gateways may include a null envelope `usage` alongside the
/// actual counters in `response.usage`. A null envelope is absence, not a value
/// that should shadow the nested response usage.
fn native_response_usage(value: &Value) -> Option<&Value> {
    value
        .get("usage")
        .filter(|usage| !usage.is_null())
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("usage"))
                .filter(|usage| !usage.is_null())
        })
}

#[cfg(test)]
pub(crate) fn log_native_usage_from_sse_chunk(
    chunk: &Bytes,
    pending: &mut Vec<u8>,
    debug_log: &DebugLog,
    request_log_id: &str,
    status: u16,
    pending_usage: &mut Option<Value>,
) -> bool {
    let mut completed = false;
    pending.extend_from_slice(chunk);
    while let Some((frame_end, delimiter_len)) = next_sse_frame_bytes(pending) {
        let frame = pending[..frame_end].to_vec();
        pending.drain(..frame_end + delimiter_len);
        let Ok(frame) = String::from_utf8(frame) else {
            continue;
        };
        log_native_usage_from_sse_frame(&frame, debug_log, request_log_id, status, pending_usage);
        completed |= native_sse_terminal(&frame) == Some(NativeSseTerminal::Completed);
    }
    completed
}

#[cfg(test)]
fn native_sse_frame_completed(frame: &str) -> bool {
    native_sse_terminal(frame) == Some(NativeSseTerminal::Completed)
}

pub(crate) fn log_native_usage_from_sse_frame(
    frame: &str,
    debug_log: &DebugLog,
    request_log_id: &str,
    status: u16,
    pending_usage: &mut Option<Value>,
) {
    let Some(data) = sse_data(frame) else {
        return;
    };
    if data == "[DONE]" {
        return;
    }
    let Ok(value) = serde_json::from_str::<Value>(&data) else {
        return;
    };
    if let Some(summary) = native_stream_debug_summary(&value) {
        debug_log.log(json!({
            "event": "upstream_stream_delta",
            "id": request_log_id,
            "backend": "responses",
            "status": status,
            "summary": summary
        }));
    }
    let usage = native_response_usage(&value);
    if let Some(usage) = usage {
        debug_log.log(json!({
            "event": "upstream_response",
            "id": request_log_id,
            "status": status,
            "success": true,
            "usage": usage
        }));
        let normalized = chat_usage_to_responses_usage(Some(usage));
        if !normalized.is_null() {
            *pending_usage = Some(normalized);
        }
    }
}

fn log_downstream_sse_frame(
    debug_log: &DebugLog,
    request_log_id: &str,
    backend: &str,
    frame: &str,
) {
    let mut event = json!({
        "event": "downstream_stream_frame",
        "id": request_log_id,
        "backend": backend,
        "summary": downstream_stream_debug_summary(frame)
    });
    if event["summary"].is_null() && !debug_log.include_stream_bodies() {
        return;
    }
    debug_log.log_stream_frame(event.take(), frame);
}

pub(crate) fn downstream_stream_debug_summary(frame: &str) -> Value {
    let Some(data) = sse_data(frame) else {
        return Value::Null;
    };
    if data == "[DONE]" {
        return json!({"done": true});
    }
    let Ok(value) = serde_json::from_str::<Value>(&data) else {
        return Value::Null;
    };
    let event_type = value.get("type").and_then(Value::as_str);
    let item_type = value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str);
    let part_type = value
        .get("part")
        .and_then(|part| part.get("type"))
        .and_then(Value::as_str);
    let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
    let part_text = value
        .get("part")
        .and_then(|part| part.get("text"))
        .and_then(Value::as_str)
        .unwrap_or("");
    json!({
        "event_type": event_type,
        "item_type": item_type,
        "part_type": part_type,
        "summary_index": value.get("summary_index").cloned().unwrap_or(Value::Null),
        "delta_chars": delta.chars().count(),
        "part_text_chars": part_text.chars().count()
    })
}

fn is_terminal_chat_finish_reason(reason: &str) -> bool {
    matches!(
        reason,
        "stop" | "length" | "tool_calls" | "content_filter" | "function_call"
    )
}

#[derive(Default)]
pub(crate) struct ChatAccum {
    pub(crate) message_item_id: Option<String>,
    reasoning_item_id: Option<String>,
    reasoning_display_header_emitted: bool,
    text: String,
    reasoning_text: String,
    reasoning_pending: String,
    tool_calls: Vec<ToolCallAccum>,
    suppress_duplicate_tool_markup: bool,
    split_concatenated_tool_call_arguments: bool,
    native_tool_call_seen: bool,
    deferred_content: Vec<String>,
    tool_markup_sanitizer: Sanitizer,
    usage: Option<Value>,
    finish_reason: Option<String>,
    tool_call_repair_invalid: bool,
    terminal_finish_seen: bool,
    observed_choice_index: Option<u64>,
}

#[derive(Default, Clone)]
pub(crate) struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
    identity_changed: bool,
    observed_id: String,
    observed_name: String,
}

struct ToolCallRepairBudget {
    remaining_calls: usize,
    remaining_argument_bytes: usize,
}

impl Default for ToolCallRepairBudget {
    fn default() -> Self {
        Self {
            remaining_calls: MAX_REPAIRED_CONCATENATED_TOOL_CALLS,
            remaining_argument_bytes: MAX_REPAIRED_TOOL_CALL_ARGUMENT_BYTES,
        }
    }
}

fn split_concatenated_tool_call_arguments(arguments: &str) -> Option<Vec<String>> {
    if arguments.len() > MAX_REPAIRED_TOOL_CALL_ARGUMENT_BYTES {
        return None;
    }

    let mut stream = serde_json::Deserializer::from_str(arguments).into_iter::<IgnoredAny>();
    let mut object_ranges = Vec::new();
    let mut object_start = 0;
    loop {
        let remaining = arguments.get(object_start..)?;
        if trim_json_whitespace(remaining).is_empty() {
            break;
        }
        if object_ranges.len() == MAX_REPAIRED_CONCATENATED_TOOL_CALLS {
            return None;
        }
        if !trim_json_whitespace_start(remaining).starts_with('{') {
            return None;
        }
        stream.next()?.ok()?;
        let object_end = stream.byte_offset();
        object_ranges.push(object_start..object_end);
        object_start = object_end;
    }

    (object_ranges.len() > 1).then(|| {
        object_ranges
            .into_iter()
            .map(|range| trim_json_whitespace(&arguments[range]).to_string())
            .collect()
    })
}

fn trim_json_whitespace(value: &str) -> &str {
    value.trim_matches(is_json_whitespace)
}

fn trim_json_whitespace_start(value: &str) -> &str {
    value.trim_start_matches(is_json_whitespace)
}

fn is_json_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\n' | '\r')
}

fn split_concatenated_tool_call_arguments_with_budget(
    arguments: &str,
    budget: &mut ToolCallRepairBudget,
) -> Option<Vec<String>> {
    if budget.remaining_calls < 2 {
        return None;
    }
    if budget.remaining_argument_bytes == 0 {
        return None;
    }
    if arguments.len() > budget.remaining_argument_bytes {
        return None;
    }
    budget.remaining_argument_bytes -= arguments.len();
    let repaired = split_concatenated_tool_call_arguments(arguments)?;
    if repaired.len() > budget.remaining_calls {
        return None;
    }
    budget.remaining_calls -= repaired.len();
    Some(repaired)
}

fn is_successful_tool_call_finish_reason(reason: Option<&str>) -> bool {
    reason.is_some_and(|reason| matches!(reason, "tool_calls" | "function_call"))
}

fn recovered_tool_call_id(upstream_id: Option<&str>, recovered_index: usize) -> String {
    if recovered_index == 0
        && let Some(upstream_id) = upstream_id.filter(|id| !id.is_empty())
    {
        return upstream_id.to_string();
    }
    generated_id("call")
}

fn nonempty_ids_are_unique<'a>(ids: impl IntoIterator<Item = Option<&'a str>>) -> bool {
    let mut seen = BTreeSet::new();
    ids.into_iter()
        .flatten()
        .filter(|id| !id.is_empty())
        .all(|id| seen.insert(id))
}

impl ChatAccum {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_tool_markup_suppression(suppress_duplicate_tool_markup: bool) -> Self {
        Self {
            suppress_duplicate_tool_markup,
            ..Self::default()
        }
    }

    fn from_chat_completion(value: &Value) -> Self {
        let mut accum = Self::default();
        if let Some(usage) = value.get("usage") {
            accum.usage = Some(chat_usage_to_responses_usage(Some(usage)));
        }
        let Some(choice) = value
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
        else {
            return accum;
        };
        let message = choice.get("message").unwrap_or(&Value::Null);
        accum.text.push_str(&chat_message_text(message));
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let mut acc = ToolCallAccum::default();
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    acc.id = id.to_string();
                }
                if let Some(name) = call
                    .get("function")
                    .and_then(|function| function.get("name"))
                    .and_then(Value::as_str)
                {
                    acc.name = name.to_string();
                }
                if let Some(arguments) = call
                    .get("function")
                    .and_then(|function| function.get("arguments"))
                    .and_then(Value::as_str)
                {
                    acc.arguments.push_str(arguments);
                }
                accum.tool_calls.push(acc);
            }
        }
        if let Some(finish_reason) = chat_finish_reason(choice) {
            accum.finish_reason = Some(finish_reason.to_string());
        } else if accum.tool_calls.iter().all(|call| call.name.is_empty()) {
            // Completed JSON text with omitted, null, or empty finish_reason
            // is the same premature-stop shape as `stop`. Explicit terminal
            // reasons (`length`, `content_filter`, `tool_calls`) are taken
            // from the field above and never rewritten.
            accum.finish_reason = Some("stop".to_string());
        }
        accum
    }

    pub(crate) fn apply_chat_chunk(&mut self, chunk: &Value) -> Vec<String> {
        let mut events = Vec::new();
        // OpenAI-compatible gateways disagree on where the streaming usage
        // chunk lives. The canonical location is the top-level `usage` of the
        // terminal frame, but several providers (and some SDK-shaped proxies)
        // nest it inside `choices[0].delta.usage` or even `choices[0].usage`
        // instead. Reading only the top-level field silently drops token
        // analytics for those providers, which is exactly the "model was used
        // but the graph shows 0 usage" symptom. Prefer the top-level field,
        // then fall back to the delta, then to the choice level. An explicit
        // `usage: null` at any of those locations must not defeat the fallback
        // to the next location, so every candidate read is filtered to
        // non-null before `or_else` falls through to the next one.
        let usage = chunk
            .get("usage")
            .filter(|u| !u.is_null())
            .or_else(|| {
                chunk
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                    .and_then(|choice| choice.get("delta"))
                    .and_then(|delta| delta.get("usage"))
                    .filter(|u| !u.is_null())
            })
            .or_else(|| {
                chunk
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                    .and_then(|choice| choice.get("usage"))
                    .filter(|u| !u.is_null())
            });
        if let Some(usage) = usage {
            self.usage = Some(chat_usage_to_responses_usage(Some(usage)));
        }
        let choices = chunk
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if choices.len() > 1 {
            self.tool_call_repair_invalid = true;
        }
        for choice in choices {
            if self.terminal_finish_seen {
                self.tool_call_repair_invalid = true;
            }
            let choice_index = match choice.get("index") {
                Some(index) => index.as_u64().unwrap_or_else(|| {
                    self.tool_call_repair_invalid = true;
                    0
                }),
                None => 0,
            };
            if self
                .observed_choice_index
                .is_some_and(|observed| observed != choice_index)
            {
                self.tool_call_repair_invalid = true;
            } else if self.observed_choice_index.is_none() {
                self.observed_choice_index = Some(choice_index);
            }
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(finish_reason) = chat_finish_reason(&choice) {
                if !is_successful_tool_call_finish_reason(Some(finish_reason)) {
                    self.tool_call_repair_invalid = true;
                }
                self.finish_reason = Some(finish_reason.to_string());
                self.terminal_finish_seen = true;
            }
            if let Some(incoming) = chat_reasoning_text(delta)
                && let Some(reasoning) =
                    reasoning_stream_delta(&self.reasoning_text, &incoming).map(str::to_string)
            {
                if self.reasoning_item_id.is_none() {
                    let item_id = generated_id("rsn");
                    self.reasoning_item_id = Some(item_id.clone());
                    events.push(sse(
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "item": {
                                "id": item_id,
                                "type": "reasoning",
                                "summary": [{"type": "summary_text", "text": ""}]
                            }
                        }),
                    ));
                    events.push(sse(
                        "response.reasoning_summary_part.added",
                        json!({
                            "type": "response.reasoning_summary_part.added",
                            "item_id": item_id,
                            "summary_index": 0,
                            "part": {"type": "summary_text", "text": ""}
                        }),
                    ));
                }
                self.reasoning_text.push_str(&reasoning);
                if !self.reasoning_display_header_emitted {
                    self.reasoning_display_header_emitted = true;
                    if !self.reasoning_text.trim_start().starts_with("**") {
                        events.push(sse(
                            "response.reasoning_summary_text.delta",
                            json!({
                                "type": "response.reasoning_summary_text.delta",
                                "item_id": self.reasoning_item_id.as_deref().unwrap_or(""),
                                "summary_index": 0,
                                "delta": REASONING_DISPLAY_HEADER
                            }),
                        ));
                    }
                }
                self.reasoning_pending.push_str(&reasoning);
                events.extend(self.take_reasoning_flush());
            }

            let content = chat_content_text(delta.get("content"));
            if !content.is_empty()
                && self.suppress_duplicate_tool_markup
                && !self.native_tool_call_seen
                && (!self.deferred_content.is_empty() || Sanitizer::may_contain_markup(&content))
            {
                self.deferred_content.push(content);
            } else if !content.is_empty() {
                if let Some(event) = self.take_reasoning_delta() {
                    events.push(event);
                }
                let content = if self.suppress_duplicate_tool_markup {
                    self.tool_markup_sanitizer.push(&content)
                } else {
                    content
                };
                self.emit_content(content, &mut events);
            }

            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                if let Some(event) = self.take_reasoning_delta() {
                    events.push(event);
                }
                for call in calls {
                    let index = match call.get("index") {
                        Some(index) => index.as_u64().unwrap_or_else(|| {
                            self.tool_call_repair_invalid = true;
                            0
                        }),
                        None => {
                            self.tool_call_repair_invalid = true;
                            0
                        }
                    } as usize;
                    if self.tool_calls.len() <= index {
                        self.tool_calls
                            .resize_with(index + 1, ToolCallAccum::default);
                    }
                    let acc = &mut self.tool_calls[index];
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        if !id.is_empty() {
                            if !acc.observed_id.is_empty() && acc.observed_id != id {
                                acc.identity_changed = true;
                            } else if acc.observed_id.is_empty() {
                                acc.observed_id = id.to_string();
                            }
                        }
                        if !id.is_empty() {
                            acc.id = id.to_string();
                        }
                    }
                    if let Some(name) = call
                        .get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                    {
                        if !name.is_empty() {
                            if !acc.observed_name.is_empty() && acc.observed_name != name {
                                acc.identity_changed = true;
                            } else if acc.observed_name.is_empty() {
                                acc.observed_name = name.to_string();
                            }
                        }
                        if !name.is_empty() {
                            acc.name = name.to_string();
                        }
                    }
                    if let Some(arguments) = call
                        .get("function")
                        .and_then(|function| function.get("arguments"))
                        .and_then(Value::as_str)
                    {
                        acc.arguments.push_str(arguments);
                    }
                }
            }
            if !self.native_tool_call_seen
                && self.tool_calls.iter().any(|call| !call.name.is_empty())
            {
                self.native_tool_call_seen = true;
                for content in std::mem::take(&mut self.deferred_content) {
                    let sanitized = self.tool_markup_sanitizer.push(&content);
                    self.emit_content(sanitized, &mut events);
                }
            }
        }
        events
    }

    fn has_semantic_terminal_finish_reason(&self) -> bool {
        self.finish_reason
            .as_deref()
            .is_some_and(is_terminal_chat_finish_reason)
    }

    pub(crate) fn finish(
        &mut self,
        response_id: &str,
        custom_tool_names: &BTreeSet<String>,
        namespace_helpers: &NamespaceHelpers,
        tool_policy: &ToolPolicyConfig,
        continue_guard: Option<(&DebugLog, &str, &ContinueGuardState)>,
    ) -> Vec<String> {
        let mut events = Vec::new();
        self.finalize_tool_markup_content(&mut events);
        if !self.reasoning_pending.is_empty() {
            events.push(self.reasoning_delta_event(&self.reasoning_pending));
        }
        if !self.reasoning_text.is_empty() {
            events.push(sse(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "id": self
                            .reasoning_item_id
                            .clone()
                            .unwrap_or_else(|| generated_id("rsn")),
                        "type": "reasoning",
                        "summary": [{"type": "summary_text", "text": self.reasoning_text}]
                    }
                }),
            ));
        }
        if !self.text.is_empty() {
            events.push(sse(
                "response.output_item.done",
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "id": self
                            .message_item_id
                            .clone()
                            .unwrap_or_else(|| generated_id("msg")),
                        "type": "message",
                        "role": "assistant",
                        "content": [{"type": "output_text", "text": self.text}]
                    }
                }),
            ));
        }

        let repair_enabled = self.split_concatenated_tool_call_arguments
            && !self.tool_call_repair_invalid
            && is_successful_tool_call_finish_reason(self.finish_reason.as_deref())
            && nonempty_ids_are_unique(
                self.tool_calls
                    .iter()
                    .map(|call| (!call.id.is_empty()).then_some(call.id.as_str())),
            );
        let mut repair_budget = ToolCallRepairBudget::default();
        for call in &self.tool_calls {
            if call.name.is_empty() {
                continue;
            }
            let repaired_arguments = (repair_enabled && !call.identity_changed)
                .then(|| {
                    split_concatenated_tool_call_arguments_with_budget(
                        &call.arguments,
                        &mut repair_budget,
                    )
                })
                .flatten();
            let arguments = repaired_arguments.as_ref().map_or_else(
                || vec![call.arguments.as_str()],
                |items| items.iter().map(String::as_str).collect(),
            );
            for (index, arguments) in arguments.into_iter().enumerate() {
                let call_id = recovered_tool_call_id(Some(&call.id), index);
                let item = tool_call_item(
                    &call.name,
                    arguments,
                    &call_id,
                    custom_tool_names,
                    namespace_helpers,
                    tool_policy,
                );
                events.push(sse(
                    "response.output_item.done",
                    json!({
                        "type": "response.output_item.done",
                        "item": item
                    }),
                ));
            }
        }

        let end_turn = self.end_turn(continue_guard);
        let mut response = json!({
            "id": response_id,
            "object": "response",
            "status": "completed",
            "end_turn": end_turn
        });
        if let Some(usage) = &self.usage
            && let Some(response) = response.as_object_mut()
        {
            response.insert("usage".to_string(), usage.clone());
        }
        events.push(sse(
            "response.completed",
            json!({
                "type": "response.completed",
                "response": response
            }),
        ));
        events.push("data: [DONE]\n\n".to_string());
        events
    }

    fn finalize_tool_markup_content(&mut self, events: &mut Vec<String>) {
        if !self.suppress_duplicate_tool_markup {
            return;
        }
        if self.native_tool_call_seen {
            let tail = self.tool_markup_sanitizer.finish();
            self.emit_content(tail, events);
        } else {
            for content in std::mem::take(&mut self.deferred_content) {
                self.emit_content(content, events);
            }
        }
    }

    #[cfg_attr(not(test), allow(dead_code))] // consumed by the route integration slice
    fn failure_events(&mut self) -> Vec<String> {
        let mut events = Vec::new();
        if let Some(event) = self.take_reasoning_delta() {
            events.push(event);
        }
        self.finalize_tool_markup_content(&mut events);
        events
    }

    fn flush_reasoning_delta(&mut self) -> String {
        let event = self.reasoning_delta_event(&self.reasoning_pending);
        self.reasoning_pending.clear();
        event
    }

    fn emit_content(&mut self, content: String, events: &mut Vec<String>) {
        if content.is_empty() {
            return;
        }
        if self.message_item_id.is_none() {
            let item_id = generated_id("msg");
            self.message_item_id = Some(item_id.clone());
            events.push(sse(
                "response.output_item.added",
                json!({
                    "type": "response.output_item.added",
                    "item": {"id": item_id, "type": "message", "role": "assistant", "content": []}
                }),
            ));
        }
        self.text.push_str(&content);
        events.push(sse(
            "response.output_text.delta",
            json!({
                "type": "response.output_text.delta", "delta": content
            }),
        ));
    }

    fn take_reasoning_delta(&mut self) -> Option<String> {
        (!self.reasoning_pending.is_empty()).then(|| self.flush_reasoning_delta())
    }

    fn take_reasoning_flush(&mut self) -> Option<String> {
        reasoning_should_flush(&self.reasoning_pending).then(|| self.flush_reasoning_delta())
    }

    fn reasoning_delta_event(&self, delta: &str) -> String {
        sse(
            "response.reasoning_summary_text.delta",
            json!({
                "type": "response.reasoning_summary_text.delta",
                "item_id": self.reasoning_item_id.as_deref().unwrap_or(""),
                "summary_index": 0,
                "delta": delta
            }),
        )
    }

    fn end_turn(&self, continue_guard: Option<(&DebugLog, &str, &ContinueGuardState)>) -> bool {
        let Some((debug_log, request_log_id, state)) = continue_guard else {
            return true;
        };
        let decision = state.decision(self);
        if decision.suspected {
            debug_log.log(json!({
                "event": "continue_guard",
                "id": request_log_id,
                "action": decision.action,
                "reason": decision.reason,
                "finish_reason": self.finish_reason,
                "tool_call_count": self.tool_calls.iter().filter(|call| !call.name.is_empty()).count(),
                "active_plan": state.active_plan,
                "unresolved_subagent": state.unresolved_subagent,
                "text_chars": self.text.chars().count(),
                "text_fingerprint": text_fingerprint(&self.text)
            }));
        }
        !decision.force_follow_up
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ContinueGuardState {
    config: ContinueGuardConfig,
    guard_key: Option<String>,
    active_plan: Option<ActivePlanSummary>,
    progress: bool,
    unresolved_subagent: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
struct ActivePlanSummary {
    pending: usize,
    in_progress: usize,
}

struct ContinueGuardDecision {
    suspected: bool,
    force_follow_up: bool,
    action: &'static str,
    reason: &'static str,
}

impl ContinueGuardState {
    pub(crate) fn from_request(config: ContinueGuardConfig, request: &Value) -> Self {
        let guard_key = request
            .get("prompt_cache_key")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let progress = request_shows_tool_progress(request);
        // Reset on inbound tool progress, not only on suspected stops: a normal
        // summary after real work must clear the consecutive-stop counter so a
        // later pause in the same session can still auto-continue.
        if progress {
            reset_continue_guard_budget(guard_key.as_deref());
        }
        Self {
            guard_key,
            active_plan: latest_active_plan(request),
            progress,
            unresolved_subagent: request_has_unresolved_subagent_work(request),
            config,
        }
    }

    fn decision(&self, accum: &ChatAccum) -> ContinueGuardDecision {
        if !self.config.enabled {
            return ContinueGuardDecision::none("disabled");
        }
        if accum.finish_reason.as_deref() != Some("stop") {
            return ContinueGuardDecision::none("finish_reason_not_stop");
        }
        if accum.tool_calls.iter().any(|call| !call.name.is_empty()) {
            return ContinueGuardDecision::none("tool_call_emitted");
        }
        // A completed `update_plan` is a wrap-up signal only when the model
        // stopped there. If the last request item is later tool work, the plan
        // snapshot is stale and must not hide a mid-task pause. Missing plans
        // never suppress: many providers never call `update_plan`.
        // Unresolved sub-agent work (spawn without a later wait) is itself a
        // mid-task signal: parents must keep going to wait/resume rather than
        // end the turn while a child is still running. This wins over a
        // completed plan snapshot, which can lag behind an open spawn wave.
        if self.unresolved_subagent {
            // fall through to mode handling below
        } else if self
            .active_plan
            .as_ref()
            .is_some_and(|plan| !plan.has_open_items())
            && !self.progress
        {
            return ContinueGuardDecision::none("plan_completed");
        } else if !looks_like_mid_task_stop(&accum.text) {
            return ContinueGuardDecision::none("assistant_text_not_continuation");
        }

        match self.config.mode {
            ContinueGuardMode::Observe => ContinueGuardDecision {
                suspected: true,
                force_follow_up: false,
                action: "observe",
                reason: "suspected_premature_stop",
            },
            ContinueGuardMode::EndTurnFalse => {
                if self.consume_followup_budget() {
                    ContinueGuardDecision {
                        suspected: true,
                        force_follow_up: true,
                        action: "end_turn_false",
                        reason: "suspected_premature_stop",
                    }
                } else {
                    ContinueGuardDecision {
                        suspected: true,
                        force_follow_up: false,
                        action: "max_followups_reached",
                        reason: "suspected_premature_stop",
                    }
                }
            }
        }
    }

    fn consume_followup_budget(&self) -> bool {
        if self.config.max_followups == 0 {
            return false;
        }
        let Some(key) = &self.guard_key else {
            return false;
        };
        let Ok(mut budgets) = continue_guard_budgets().lock() else {
            return false;
        };
        evict_continue_guard_budgets_if_needed(&mut budgets);
        let used = budgets.entry(key.clone()).or_insert(0);
        if *used >= self.config.max_followups {
            return false;
        }
        *used += 1;
        true
    }
}

impl ContinueGuardDecision {
    fn none(reason: &'static str) -> Self {
        Self {
            suspected: false,
            force_follow_up: false,
            action: "none",
            reason,
        }
    }
}

impl ActivePlanSummary {
    fn has_open_items(&self) -> bool {
        self.pending > 0 || self.in_progress > 0
    }
}

fn continue_guard_budgets() -> &'static Mutex<BTreeMap<String, u8>> {
    static BUDGETS: OnceLock<Mutex<BTreeMap<String, u8>>> = OnceLock::new();
    BUDGETS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn reset_continue_guard_budget(key: Option<&str>) {
    let Some(key) = key else {
        return;
    };
    let Ok(mut budgets) = continue_guard_budgets().lock() else {
        return;
    };
    budgets.remove(key);
}

/// Evict oldest entries from the budget map when it exceeds the size cap to
/// prevent unbounded memory growth during long-running proxy sessions.
fn evict_continue_guard_budgets_if_needed(budgets: &mut BTreeMap<String, u8>) {
    if budgets.len() <= CONTINUE_GUARD_BUDGET_MAX_ENTRIES {
        return;
    }
    // Remove approximately 10% of entries to amortize eviction cost.
    let target = budgets.len() * 9 / 10;
    while budgets.len() > target {
        budgets.pop_first();
    }
}

fn latest_active_plan(request: &Value) -> Option<ActivePlanSummary> {
    let mut plans = Vec::new();
    collect_update_plan_arguments(request, &mut plans);
    plans.into_iter().filter_map(parse_plan_summary).next_back()
}

fn collect_update_plan_arguments(value: &Value, plans: &mut Vec<Value>) {
    match value {
        Value::Object(object) => {
            if object.get("name").and_then(Value::as_str) == Some("update_plan")
                && let Some(arguments) = object.get("arguments")
            {
                plans.push(arguments.clone());
            }
            if object
                .get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                == Some("update_plan")
                && let Some(arguments) = object
                    .get("function")
                    .and_then(|function| function.get("arguments"))
            {
                plans.push(arguments.clone());
            }
            for child in object.values() {
                collect_update_plan_arguments(child, plans);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_update_plan_arguments(item, plans);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn parse_plan_summary(arguments: Value) -> Option<ActivePlanSummary> {
    let value = match arguments {
        Value::String(arguments) => serde_json::from_str::<Value>(&arguments).ok()?,
        value => value,
    };
    let plan = value.get("plan").and_then(Value::as_array)?;
    let mut summary = ActivePlanSummary::default();
    for item in plan {
        match item.get("status").and_then(Value::as_str) {
            Some("pending") => summary.pending += 1,
            Some("in_progress") => summary.in_progress += 1,
            _ => {}
        }
    }
    Some(summary)
}

/// True when the last request item is completed tool work (or a pending
/// non-plan tool call). The continue-guard budget resets on this signal so
/// `max_followups` caps only consecutive text-only stops rather than every
/// mid-task stop in a long session. `update_plan` is planning, not progress:
/// treating it as tool work would reset the budget on every plan-only turn
/// and let text-only pause loops run past `max_followups`. Tool outputs and
/// chat `role=tool` messages count only when they match a non-plan call id
/// already in the request; missing or unmatched ids are not progress.
fn request_shows_tool_progress(request: &Value) -> bool {
    if let Some(input) = request.get("input").and_then(Value::as_array) {
        return input
            .last()
            .is_some_and(|item| item_shows_tool_progress(item, input));
    }
    // Defensive chat-completions shape (some callers may pass a converted body):
    // tool results or pending assistant tool calls at the end of `messages`.
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| chat_messages_show_tool_progress(messages))
}

/// True when the request history still has sub-agent spawn work that was never
/// followed by a wait. Parents that stop after spawning without waiting leave
/// children running and force the user to prompt "continue"; treat that shape
/// as a premature stop even when the assistant text is terse.
fn request_has_unresolved_subagent_work(request: &Value) -> bool {
    if let Some(input) = request.get("input").and_then(Value::as_array) {
        return input_has_unresolved_subagent_work(input);
    }
    request
        .get("messages")
        .and_then(Value::as_array)
        .is_some_and(|messages| chat_messages_have_unresolved_subagent_work(messages))
}

fn input_has_unresolved_subagent_work(items: &[Value]) -> bool {
    let mut open_spawns = 0i32;
    apply_subagent_lifecycle_items(items, &mut open_spawns);
    open_spawns > 0
}

fn chat_messages_have_unresolved_subagent_work(messages: &[Value]) -> bool {
    let mut open_spawns = 0i32;
    for message in messages {
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            apply_subagent_lifecycle_items(calls, &mut open_spawns);
        }
        apply_subagent_lifecycle_item(message, &mut open_spawns);
    }
    open_spawns > 0
}

fn apply_subagent_lifecycle_items(items: &[Value], open_spawns: &mut i32) {
    for item in items {
        apply_subagent_lifecycle_item(item, open_spawns);
    }
}

fn apply_subagent_lifecycle_item(item: &Value, open_spawns: &mut i32) {
    match item_subagent_lifecycle(item) {
        SubagentLifecycle::Spawn => *open_spawns += 1,
        // A wait acknowledges outstanding children for this parent turn.
        // Codex may return on the first completed target; later text may
        // still need follow-up, but that is handled by the mid-task text
        // classifier. Clearing here avoids double-counting completed waves.
        SubagentLifecycle::Wait => *open_spawns = 0,
        SubagentLifecycle::Neither => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubagentLifecycle {
    Spawn,
    Wait,
    Neither,
}

fn item_subagent_lifecycle(item: &Value) -> SubagentLifecycle {
    let name = item
        .get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            item.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
        })
        .unwrap_or("");
    let bare = name.rsplit('.').next().unwrap_or(name);
    match bare {
        "spawn_agent" | "spawn" => SubagentLifecycle::Spawn,
        "wait_agent" | "wait_threads" | "wait" => SubagentLifecycle::Wait,
        _ => SubagentLifecycle::Neither,
    }
}

fn item_shows_tool_progress(item: &Value, items: &[Value]) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call_output" | "custom_tool_call_output") => {
            output_matches_non_plan_call(item, items)
        }
        Some("function_call" | "tool_call" | "custom_tool_call") => !item_is_update_plan(item),
        _ => false,
    }
}

fn output_matches_non_plan_call(output: &Value, items: &[Value]) -> bool {
    let Some(call_id) = output_call_id(output) else {
        return false;
    };
    items.iter().any(|item| {
        matches!(
            item.get("type").and_then(Value::as_str),
            Some("function_call" | "tool_call" | "custom_tool_call")
        ) && !item_is_update_plan(item)
            && call_item_id(item).is_some_and(|id| id == call_id)
    })
}

fn output_call_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .and_then(Value::as_str)
        .or_else(|| item.get("tool_call_id").and_then(Value::as_str))
}

fn item_is_update_plan(item: &Value) -> bool {
    item.get("name").and_then(Value::as_str) == Some("update_plan")
        || item
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            == Some("update_plan")
}

fn chat_messages_show_tool_progress(messages: &[Value]) -> bool {
    let Some(last) = messages.last() else {
        return false;
    };
    if last.get("role").and_then(Value::as_str) == Some("tool") {
        return tool_message_matches_non_plan_call(last, messages);
    }
    last.get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| calls.iter().any(|call| !item_is_update_plan(call)))
}

fn tool_message_matches_non_plan_call(message: &Value, messages: &[Value]) -> bool {
    let Some(call_id) = output_call_id(message) else {
        return false;
    };
    messages.iter().rev().skip(1).any(|message| {
        message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| {
                calls.iter().any(|call| {
                    !item_is_update_plan(call) && call_item_id(call).is_some_and(|id| id == call_id)
                })
            })
    })
}

fn call_item_id(item: &Value) -> Option<&str> {
    item.get("call_id")
        .and_then(Value::as_str)
        .or_else(|| item.get("id").and_then(Value::as_str))
}

fn looks_like_mid_task_stop(text: &str) -> bool {
    let normalized = text
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() {
        return false;
    }
    // Ranked classifier:
    // 1. First-person / let-me prefixes: after stripping adverbs and nested
    //    prefixes, the next action is work when it is a known work verb, or
    //    an unlisted verb with a concrete object ("I'll clone the repo",
    //    "I'll add tests"). Particles in the complement ("back", "ahead",
    //    "up") are stripped before the object is classified, so "check back
    //    with you" is a hand-off and "follow up soon" is not a work object.
    //    Discourse verbs such as "know"/"see"/"help" are not wrap-up vetoes:
    //    "let me know" and "see if you" stay hand-offs, while "know what
    //    failed in the test output" / "see the test output" / "see if the
    //    tests pass" / "help fix" still continue. `if`/`whether`/`when` are
    //    person hand-offs only when the clause addresses the user, not when
    //    they introduce speaker work. Wrap-up verbs, person complements,
    //    offer clauses, leftover adverbs/pronouns, light nouns plus
    //    deferral, and work verbs whose only complement is postponement do
    //    not count ("Now let me summarize", "I'll update you",
    //    "I'll do it next", "I'll sit tight", "I'll take another look later",
    //    "look at your PR", "I'll continue later", "I'll run soon",
    //    "I'll wait", "I'll wait later"). "Let me wait for the agent" and
    //    sub-agent resume verbs ("get"/"resume"/"collect") still continue.
    //    Immediacy ("I'll verify now", "I'll continue") is still work.
    // 2. Wrap-up / hand-off phrasing. This loses to a prefix+work-action pair
    //    so "Thanks to the rebase. Now let me verify" still continues, and
    //    "no actionable issues. Now let me audit file B" still continues.
    // 3. Dangling `:`/`...` only when the last sentence still talks about
    //    unfinished speaker work. Delivery frames ("Here is a summary of
    //    remaining work:") are not pauses. Remaining/pending polarity is
    //    per clause, not a sentence veto: "No issues remaining:" stays
    //    done, "Nothing pending, but I still need to:" still continues,
    //    and "Nothing pending, verification is pending:" still continues
    //    because the copular pending clause is not itself cleared. Bare
    //    `pending` is a status label ("Review pending:", "Approval
    //    pending:"), not speaker work; speaker pending uses a copula
    //    ("This is still pending:", "verification is pending:").
    //    Remaining is predicative ("Tasks remaining:", "still remaining")
    //    or a clause header after comma/`but`/`yet` ("Remaining tasks:",
    //    "The remaining items:", "All remaining tasks:",
    //    "Incomplete remaining tasks:", "Complete remaining tasks:",
    //    "Summary, remaining tasks:"), not an
    //    attributive noun modifier inside an `and`-coordinated phrase
    //    ("Summary and remaining tasks:") and not a remaining subject
    //    whose copular predicate is completion ("Remaining work is complete:",
    //    "Remaining tasks are done:"), not an attributive complete
    //    ("Remaining complete tasks:") or a hedged predicate
    //    ("Remaining tasks are mostly done:"). Presentational copulas
    //    ("Here are the remaining items:", "Below are remaining tasks:",
    //    "Above are the remaining steps:", "Following are remaining tasks:")
    //    stay delivery even when remaining appears later in the sentence.
    if contains_work_intent(&normalized) {
        return true;
    }
    if contains_wrap_up_closing_phrase(&normalized) {
        return false;
    }
    dangling_punctuation_with_remaining_work(&normalized)
}

fn contains_work_intent(normalized: &str) -> bool {
    const FIRST_PERSON_PREFIXES: [&str; 7] = [
        "let me ",
        "i'll ",
        "i will ",
        "i still need to ",
        "i need to ",
        "i'm going to ",
        "i should ",
    ];
    // Sequencing words are common in hand-offs ("Next I need a decision from
    // you"). They count only when the next action is a known work verb, after
    // nested first-person fillers. "Then I'll clone the repo" still matches
    // via the first-person prefix, not via `then`.
    const SEQUENCING_PREFIXES: [&str; 2] = ["then ", "next "];
    prefix_has_work_intent(normalized, &FIRST_PERSON_PREFIXES, false)
        || prefix_has_work_intent(normalized, &SEQUENCING_PREFIXES, true)
}

fn prefix_has_work_intent(
    normalized: &str,
    prefixes: &[&str],
    require_known_work_verb: bool,
) -> bool {
    prefixes.iter().any(|prefix| {
        let mut start = 0;
        while let Some(idx) = normalized[start..].find(prefix) {
            let after_prefix = strip_intent_fillers(&normalized[start + idx + prefix.len()..]);
            let matched = if require_known_work_verb {
                remainder_starts_with_work_verb(after_prefix)
            } else {
                remainder_is_work_action(after_prefix)
            };
            if matched {
                return true;
            }
            start += idx + prefix.len();
        }
        false
    })
}

fn strip_intent_fillers(mut rest: &str) -> &str {
    loop {
        let Some(next) = [
            "just ",
            "also ",
            "first ",
            "now ",
            "next ",
            "still ",
            "quickly ",
            "please ",
            "then ",
            "try ",
            "help ",
            "to ",
            "let me ",
            "i'll ",
            "i will ",
            "i still need to ",
            "i need to ",
            "i'm going to ",
            "i should ",
        ]
        .iter()
        .find_map(|filler| rest.strip_prefix(filler)) else {
            // Observed pauses use "re-audit" rather than a catalogued stem.
            // Strip a hyphenated repetition prefix so the action check sees
            // "audit", without treating "read" as "ad".
            if let Some(stripped) = rest.strip_prefix("re-") {
                rest = stripped;
                continue;
            }
            return rest;
        };
        rest = next;
    }
}

fn remainder_is_work_action(rest: &str) -> bool {
    if rest.is_empty() || remainder_starts_with_wrap_up_action(rest) {
        return false;
    }
    let complement = strip_complement_fillers(action_complement(rest));
    // Person heads and person-addressing *if/whether/when* clauses are
    // hand-offs even for work verbs ("look at your PR", "check if you
    // need"). Trailing offer clauses after a real object still continue
    // ("I'll inspect the tree if you want") because those do not open the
    // complement. "Let me know if …" is an inform-me request, not speaker
    // work, even without "you".
    if complement_is_person_hand_off(complement) || remainder_is_inform_conditional(rest) {
        return false;
    }
    // Known work verbs may stand alone ("Let me check.") and may take a
    // pronoun object ("I'll inspect it next"). A work verb whose only
    // complement is postponement is not the next action
    // ("I'll continue later", "I'll run soon", "I'll continue next").
    // Immediacy is still work ("I'll verify now", "I'll continue").
    // Unlisted verbs need a concrete noun object
    // ("I'll clone the repo", "I'll add tests"), not a leftover pronoun,
    // time adverb, state adjective, offer clause, or light noun plus
    // deferral ("I'll do it next", "I'll follow up soon", "I'll sit tight",
    // "I'll take another look later"). Discourse verbs such as "know"/"see"
    // stay hand-offs when the complement still addresses the user
    // ("Let me know what you'd like next") and continue when it names
    // speaker work ("Let me see if the tests pass").
    if remainder_starts_with_work_verb(rest) {
        return !complement_is_deferral_only(complement);
    }
    if complement_addresses_person(complement) {
        return false;
    }
    complement_is_concrete_object(complement)
}

fn action_complement(rest: &str) -> &str {
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    rest[end..].trim_start()
}

fn strip_complement_fillers(mut complement: &str) -> &str {
    loop {
        let Some(next) = [
            "back ", "ahead ", "along ", "again ", "around ", "up ", "out ", "off ", "down ",
            "at ", "in ", "on ", "into ", "from ", "with ", "for ", "of ", "to ",
        ]
        .iter()
        .find_map(|filler| complement.strip_prefix(filler)) else {
            return complement;
        };
        complement = next;
    }
}

fn complement_head(complement: &str) -> &str {
    let end = complement
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(complement.len());
    &complement[..end]
}

fn complement_is_person_hand_off(complement: &str) -> bool {
    matches!(
        complement_head(complement),
        "you" | "your" | "yourself" | "about" | "here"
    ) || complement_opens_with_person_clause(complement)
}

fn complement_opens_with_person_clause(complement: &str) -> bool {
    matches!(complement_head(complement), "if" | "whether" | "when")
        && complement_addresses_person(complement)
}

fn remainder_is_inform_conditional(rest: &str) -> bool {
    token_starts_with_stem(rest, "know")
        && matches!(
            complement_head(strip_complement_fillers(action_complement(rest))),
            "if" | "whether" | "when"
        )
}

fn complement_is_deferral_only(complement: &str) -> bool {
    // Postponement of this turn ("later", "soon", "next") is not work.
    // Immediacy tokens ("now", "still") stay in `is_deferral_token` so
    // unlisted verbs can peel them off an object ("do it now"), but a
    // catalogued work verb plus only "now" is the next action.
    // Trailing punctuation is not postponement: "I'll continue." is a
    // bare work verb.
    let stripped = complement
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && !c.is_ascii_whitespace())
        .trim();
    !stripped.is_empty() && peel_trailing_postponement(stripped).is_empty()
}

fn complement_addresses_person(complement: &str) -> bool {
    complement
        .split(|c: char| !c.is_ascii_alphabetic())
        .any(|token| matches!(token, "you" | "your" | "yours" | "yourself"))
}

fn complement_is_concrete_object(complement: &str) -> bool {
    let complement = normalize_unlisted_verb_complement(complement);
    if complement.is_empty()
        || complement_is_generic_pronoun(complement)
        || complement_is_person_hand_off(complement)
        || complement_is_non_object_head(complement)
        || complement_has_offer_clause(complement)
        || complement_is_light_noun_without_object(complement)
    {
        return false;
    }
    true
}

fn normalize_unlisted_verb_complement(mut complement: &str) -> &str {
    loop {
        let stripped = strip_complement_fillers(strip_leading_determiners(complement));
        let peeled = peel_trailing_deferral(stripped);
        if peeled == complement {
            return peeled;
        }
        complement = peeled;
    }
}

fn strip_leading_determiners(mut complement: &str) -> &str {
    loop {
        let Some(next) = [
            "a ", "an ", "the ", "another ", "some ", "any ", "more ", "one ",
        ]
        .iter()
        .find_map(|determiner| complement.strip_prefix(determiner)) else {
            return complement;
        };
        complement = next;
    }
}

fn peel_trailing_deferral(complement: &str) -> &str {
    peel_trailing_matching_tokens(complement, is_deferral_token)
}

fn peel_trailing_postponement(complement: &str) -> &str {
    peel_trailing_matching_tokens(complement, is_postponement_token)
}

fn peel_trailing_matching_tokens(complement: &str, is_token: fn(&str) -> bool) -> &str {
    let mut complement = complement
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && !c.is_ascii_whitespace());
    loop {
        let last = complement
            .rsplit(|c: char| c.is_ascii_whitespace() || !c.is_ascii_alphabetic())
            .find(|token| !token.is_empty())
            .unwrap_or("");
        if last.is_empty() || !is_token(last) {
            return complement.trim_end();
        }
        let Some(end) = complement.rfind(last) else {
            return complement.trim_end();
        };
        complement = complement[..end].trim_end();
    }
}

fn is_postponement_token(token: &str) -> bool {
    matches!(
        token,
        "soon"
            | "later"
            | "today"
            | "tomorrow"
            | "tonight"
            | "afterwards"
            | "instead"
            | "anyway"
            | "already"
            | "currently"
            | "next"
    )
}

fn is_deferral_token(token: &str) -> bool {
    is_postponement_token(token) || matches!(token, "now" | "still")
}

fn complement_is_light_noun_without_object(complement: &str) -> bool {
    if !matches!(
        complement_head(complement),
        "look" | "glance" | "peek" | "moment" | "break"
    ) {
        return false;
    }
    let after_noun = action_complement(complement);
    let object = normalize_unlisted_verb_complement(after_noun);
    object.is_empty()
        || complement_is_generic_pronoun(object)
        || complement_is_person_hand_off(object)
        || complement_is_non_object_head(object)
}

fn complement_is_generic_pronoun(complement: &str) -> bool {
    matches!(
        complement_head(complement),
        "it" | "this" | "that" | "them" | "these" | "those" | "something" | "anything"
    )
}

fn complement_is_non_object_head(complement: &str) -> bool {
    matches!(
        complement_head(complement),
        "soon"
            | "later"
            | "now"
            | "today"
            | "tomorrow"
            | "tonight"
            | "afterwards"
            | "instead"
            | "anyway"
            | "already"
            | "currently"
            | "tight"
            | "quiet"
            | "there"
            | "once"
            | "still"
    )
}

fn complement_has_offer_clause(complement: &str) -> bool {
    complement.starts_with("if you")
        || complement.starts_with("when you")
        || complement.starts_with("whenever you")
        || complement.contains(" if you")
        || complement.contains(" when you")
        || complement.contains(" whenever you")
}

fn remainder_starts_with_wrap_up_action(rest: &str) -> bool {
    // Bare "wait" / "I'll wait later" is a pause. "Let me wait for the agent"
    // or "wait until the tests finish" is still speaker work and must continue.
    if token_starts_with_stem(rest, "wait") {
        let complement = strip_complement_fillers(action_complement(rest));
        let complement = complement
            .trim_end_matches(|c: char| !c.is_ascii_alphanumeric() && !c.is_ascii_whitespace())
            .trim();
        return complement.is_empty() || complement_is_deferral_only(complement);
    }
    [
        "summarize",
        "stop",
        "leave",
        "wrap",
        "explain",
        "tell",
        "pause",
        "recap",
        "conclude",
        "stay",
        "remain",
        "think",
        "note",
        "rest",
    ]
    .iter()
    .any(|stem| token_starts_with_stem(rest, stem))
}

fn remainder_starts_with_work_verb(rest: &str) -> bool {
    // Include sub-agent resume verbs observed in long multi-agent sessions
    // ("let me get Avicenna's findings by resuming it", "I'll collect results").
    // "get back to you" stays a hand-off via person-complement detection after
    // particle stripping.
    const STEMS: [&str; 41] = [
        "check", "inspect", "look", "read", "write", "run", "verify", "open", "search", "audit",
        "push", "apply", "test", "fix", "review", "examine", "fetch", "pull", "grep", "list",
        "continue", "start", "compare", "confirm", "dump", "patch", "edit", "find", "scan",
        "rebase", "commit", "merge", "build", "checkout", "resume", "get", "collect", "gather",
        "retrieve", "obtain", "wait",
    ];
    STEMS.iter().any(|stem| token_starts_with_stem(rest, stem))
}

fn token_starts_with_stem(rest: &str, stem: &str) -> bool {
    let Some(after) = rest.strip_prefix(stem) else {
        return false;
    };
    let suffix_end = after
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(after.len());
    matches!(&after[..suffix_end], "" | "s" | "es" | "ed" | "ing")
}

fn dangling_punctuation_with_remaining_work(normalized: &str) -> bool {
    if !(normalized.ends_with(':') || normalized.ends_with("...") || normalized.ends_with('…')) {
        return false;
    }
    let last_sentence = normalized
        .rsplit(['.', '!', '?', ';'])
        .next()
        .unwrap_or(normalized)
        .trim();
    if last_sentence_is_delivery(last_sentence) {
        return false;
    }
    last_sentence_has_unfinished_speaker_work(last_sentence)
}

fn last_sentence_has_unfinished_speaker_work(last_sentence: &str) -> bool {
    // Comma/`but`/`yet` introduce independent clauses, so a later remaining
    // header still counts ("Summary, remaining tasks:", "Nothing pending,
    // remaining tasks:"). `and` only coordinates noun phrases, so
    // "remaining tasks" after `and` stays attributive.
    independent_clauses(last_sentence).any(|clause| {
        remaining_opens_unfinished_header(clause)
            || coordinated_conjuncts(clause).any(clause_has_unfinished_speaker_work)
    })
}

fn remaining_opens_unfinished_header(clause: &str) -> bool {
    if clause_clears_remaining_work(clause) {
        return false;
    }
    // Remaining headers allow remaining-NP premodifiers: determiners/
    // quantifiers plus a status adjective ("the remaining items",
    // "all remaining tasks", "incomplete remaining tasks",
    // "complete remaining tasks"). Do not skip other nouns: "summary
    // remaining" is not a header, and `and`-coordination is handled by
    // scoring remaining only at the independent-clause head
    // ("Summary and remaining tasks:"). Copular completion still wins
    // ("Complete remaining tasks are done:").
    remaining_header_head(clause) == "remaining"
}

fn remaining_header_head(clause: &str) -> &str {
    clause_alpha_tokens(clause)
        .find(|token| !is_remaining_header_premodifier(token))
        .unwrap_or("")
}

fn is_remaining_header_premodifier(token: &str) -> bool {
    is_remaining_np_determiner(token) || is_remaining_np_status_adjective(token)
}

fn is_remaining_np_determiner(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "the"
            | "another"
            | "some"
            | "any"
            | "more"
            | "one"
            | "all"
            | "both"
            | "each"
            | "every"
            | "few"
            | "several"
            | "many"
            | "most"
    )
}

fn is_remaining_np_status_adjective(token: &str) -> bool {
    matches!(
        token,
        "incomplete"
            | "unfinished"
            | "outstanding"
            | "leftover"
            | "open"
            | "pending"
            | "complete"
            | "completed"
    )
}

fn clause_alpha_tokens(clause: &str) -> impl Iterator<Item = &str> {
    clause
        .split(|c: char| !c.is_alphabetic())
        .filter(|token| !token.is_empty())
}

fn independent_clauses(last_sentence: &str) -> impl Iterator<Item = &str> {
    last_sentence
        .split(',')
        .flat_map(|part| part.split(" but "))
        .flat_map(|part| part.split(" yet "))
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

fn coordinated_conjuncts(clause: &str) -> impl Iterator<Item = &str> {
    clause
        .split(" and ")
        .map(str::trim)
        .filter(|part| !part.is_empty())
}

fn clause_has_unfinished_speaker_work(clause: &str) -> bool {
    const SPEAKER_WORK_CUES: [&str; 8] = [
        "still need",
        "still have",
        "next step",
        "after that",
        "not yet",
        "to do",
        "follow up",
        "follow-up",
    ];
    if SPEAKER_WORK_CUES.iter().any(|cue| clause.contains(cue)) {
        return true;
    }
    if clause_clears_remaining_work(clause) {
        return false;
    }
    // Copular pending is speaker status ("this is still pending",
    // "verification is pending"). Bare "pending" is a label on some other
    // actor or process ("review pending", "approval pending", "ci pending").
    // Remaining is unfinished only as a predicate ("tasks remaining",
    // "work is remaining"), not as a modifier ("remaining tasks") that
    // `and`-coordination would otherwise promote into a fake header.
    clause.contains("still pending")
        || clause.contains("is pending")
        || clause.contains("are pending")
        || remaining_is_predicative(clause)
}

fn remaining_is_predicative(clause: &str) -> bool {
    clause.contains("still remaining")
        || clause.contains("is remaining")
        || clause.contains("are remaining")
        || clause_last_alpha_token(clause) == "remaining"
}

fn clause_last_alpha_token(clause: &str) -> &str {
    clause
        .rsplit(|c: char| !c.is_alphabetic())
        .find(|token| !token.is_empty())
        .unwrap_or("")
}

fn clause_clears_remaining_work(clause: &str) -> bool {
    // Remaining/pending cues mean unfinished speaker work unless this
    // clause negates those cues ("No issues remaining:", "nothing pending:")
    // or the remaining subject has a copular completion predicate
    // ("Remaining work is complete:", "work remaining is done:").
    // Attributive complete ("Remaining complete tasks:") and hedged
    // completion ("Remaining tasks are mostly done:") stay unfinished.
    // Do not treat generic "not" as clearance: "not yet" and
    // "Remaining work is not done:" stay unfinished. Token matching keeps
    // "incomplete" from counting as "complete".
    ["no ", "none ", "nothing ", "without ", "zero "]
        .iter()
        .any(|negation| clause.contains(negation))
        || clause_resolves_remaining_work(clause)
}

fn clause_resolves_remaining_work(clause: &str) -> bool {
    let tokens = clause_alpha_tokens(clause).collect::<Vec<_>>();
    let Some(remaining_at) = tokens.iter().position(|token| *token == "remaining") else {
        return false;
    };
    let mut negated = false;
    let mut seen_copula = false;
    let mut weakened = false;
    for token in &tokens[remaining_at + 1..] {
        if *token == "will" {
            return false;
        }
        if matches!(*token, "not" | "never" | "incomplete") {
            negated = true;
            continue;
        }
        if matches!(*token, "is" | "are" | "was" | "were" | "been" | "be") {
            seen_copula = true;
            continue;
        }
        if !seen_copula {
            continue;
        }
        if matches!(
            *token,
            "mostly"
                | "almost"
                | "nearly"
                | "partially"
                | "partly"
                | "somewhat"
                | "mainly"
                | "roughly"
        ) {
            weakened = true;
            continue;
        }
        if matches!(
            *token,
            "still" | "now" | "already" | "fully" | "currently" | "all" | "quite"
        ) {
            continue;
        }
        if matches!(*token, "complete" | "completed" | "done" | "finished") {
            return !negated && !weakened;
        }
        return false;
    }
    false
}

fn last_sentence_is_delivery(last_sentence: &str) -> bool {
    starts_with_presentational_copula(last_sentence)
        || last_sentence.contains("summary of ")
        || last_sentence.contains("final report")
}

fn starts_with_presentational_copula(sentence: &str) -> bool {
    ["here", "below", "above", "following"]
        .iter()
        .any(|locative| {
            let Some(rest) = sentence.strip_prefix(locative) else {
                return false;
            };
            rest.starts_with("'s ") || rest.starts_with(" is ") || rest.starts_with(" are ")
        })
}

/// Wrap-up phrasing that should not force a follow-up unless a prefix is
/// followed by a work action. Generic "let me"/"I'll"/"I need to"/"I should"
/// are not enough on their own, even with "now"/"first"/"still". Subtask
/// completion words such as "done" or "complete" are deliberately excluded:
/// mid-task text routinely says "the rebase is complete" before continuing
/// ("Now let me push..."). "let me know" is classified from the action after
/// the prefix, not as a substring closer, so investigative phrasing can still
/// continue.
fn contains_wrap_up_closing_phrase(normalized: &str) -> bool {
    [
        "thank you",
        "thanks",
        "feel free",
        "that's all",
        "that is all",
        "no actionable issues",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

#[cfg_attr(not(test), allow(dead_code))] // tests call this default-policy wrapper
pub(crate) fn chat_json_to_responses(value: Value, custom_tool_names: &BTreeSet<String>) -> Value {
    chat_json_to_responses_with_policy(
        value,
        custom_tool_names,
        &NamespaceHelpers::default(),
        &ToolPolicyConfig::default(),
        None,
    )
}

pub(crate) fn chat_json_to_responses_with_policy(
    value: Value,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
    continue_guard: Option<(&DebugLog, &str, &ContinueGuardState)>,
) -> Value {
    chat_json_to_responses_with_tool_markup_suppression(
        value,
        custom_tool_names,
        namespace_helpers,
        tool_policy,
        continue_guard,
        false,
        false,
    )
}

pub(crate) fn chat_json_to_responses_with_tool_markup_suppression(
    value: Value,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
    continue_guard: Option<(&DebugLog, &str, &ContinueGuardState)>,
    suppress_duplicate_tool_markup: bool,
    split_concatenated_tool_call_arguments_enabled: bool,
) -> Value {
    let value = chat_completion_payload(&value);
    let response_id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| generated_id("resp"));

    let mut output = Vec::new();
    if let Some(choices) = value.get("choices").and_then(Value::as_array)
        && let Some(choice) = choices.first()
        && let Some(message) = choice.get("message")
    {
        let reasoning = chat_reasoning_text(message);
        let native_tool_call_seen = message
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| {
                calls.iter().any(|call| {
                    call.get("function")
                        .and_then(|function| function.get("name"))
                        .and_then(Value::as_str)
                        .is_some_and(|name| !name.is_empty())
                })
            });
        let mut content = chat_message_text(message);
        let original_content_was_empty = content.is_empty();
        if suppress_duplicate_tool_markup && native_tool_call_seen {
            content = sanitized_chat_message_text(message);
        }
        let content_suppressed = !original_content_was_empty && content.is_empty();
        let mut message_parts = Vec::new();
        if let Some(reasoning) = reasoning
            && !reasoning.is_empty()
        {
            message_parts.push(json!({"type": "reasoning_summary_text", "text": reasoning}));
        }
        if !content.is_empty() {
            message_parts.push(json!({"type": "output_text", "text": content}));
        }
        if !message_parts.is_empty() {
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": message_parts
            }));
        } else if message.get("content").is_some() && !content_suppressed {
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": []
            }));
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            let repair_enabled = split_concatenated_tool_call_arguments_enabled
                && choices.len() == 1
                && choice
                    .get("index")
                    .is_none_or(|index| index.as_u64().is_some())
                && is_successful_tool_call_finish_reason(chat_finish_reason(choice))
                && nonempty_ids_are_unique(
                    calls
                        .iter()
                        .map(|call| call.get("id").and_then(Value::as_str)),
                );
            let mut repair_budget = ToolCallRepairBudget::default();
            for call in calls {
                let upstream_name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str);
                let name = upstream_name.unwrap_or("tool");
                let arguments = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let upstream_call_id = call.get("id").and_then(Value::as_str);
                let repaired_arguments = (repair_enabled
                    && upstream_name.is_some_and(|name| !name.is_empty()))
                .then(|| {
                    split_concatenated_tool_call_arguments_with_budget(
                        arguments,
                        &mut repair_budget,
                    )
                })
                .flatten();
                let arguments = repaired_arguments.as_ref().map_or_else(
                    || vec![arguments],
                    |items| items.iter().map(String::as_str).collect(),
                );
                for (index, arguments) in arguments.into_iter().enumerate() {
                    let call_id = recovered_tool_call_id(upstream_call_id, index);
                    output.push(tool_call_item(
                        name,
                        arguments,
                        &call_id,
                        custom_tool_names,
                        namespace_helpers,
                        tool_policy,
                    ));
                }
            }
        }
    }

    let end_turn = ChatAccum::from_chat_completion(value).end_turn(continue_guard);
    json!({
        "id": response_id,
        "object": "response",
        "status": "completed",
        "end_turn": end_turn,
        "output": output,
        "usage": chat_usage_to_responses_usage(value.get("usage"))
    })
}

fn chat_finish_reason(choice: &Value) -> Option<&str> {
    choice
        .get("finish_reason")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
}

fn chat_message_text(message: &Value) -> String {
    chat_content_text(message.get("content"))
}

fn sanitized_chat_message_text(message: &Value) -> String {
    let mut sanitizer = Sanitizer::default();
    match message.get("content") {
        Some(Value::String(text)) => {
            let mut content = sanitizer.push(text);
            content.push_str(&sanitizer.finish());
            content
        }
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for part in items.iter().filter_map(chat_content_part_text) {
                let sanitized = sanitizer.push(part);
                if !sanitized.is_empty() {
                    parts.push(sanitized);
                }
            }
            let tail = sanitizer.finish();
            if !tail.is_empty() {
                parts.push(tail);
            }
            let parts = parts.iter().map(String::as_str).collect::<Vec<_>>();
            join_chat_content_parts(&parts)
        }
        _ => String::new(),
    }
}

fn chat_content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => {
            let parts = items
                .iter()
                .filter_map(chat_content_part_text)
                .collect::<Vec<_>>();
            join_chat_content_parts(&parts)
        }
        _ => String::new(),
    }
}

fn join_chat_content_parts(parts: &[&str]) -> String {
    let mut text = String::new();
    for part in parts {
        if part.is_empty() {
            continue;
        }
        if content_parts_need_separator(&text, part) {
            text.push(' ');
        }
        text.push_str(part);
    }
    text
}

fn content_parts_need_separator(left: &str, right: &str) -> bool {
    if left.ends_with(char::is_whitespace) || right.starts_with(char::is_whitespace) {
        return false;
    }
    let Some(prev) = left.chars().next_back() else {
        return false;
    };
    let Some(next) = right.chars().next() else {
        return false;
    };
    // Glue hyphenated/contracted fragments ("re-" + "audit"). Insert a space
    // before a new word after letters *or* sentence punctuation ("Done." +
    // "Now let me"), so array parts stay readable and continue-guard prefixes
    // still tokenize.
    if matches!(prev, '-' | '\'') || matches!(next, '-' | '\'') {
        return false;
    }
    next.is_alphanumeric()
}

fn chat_content_part_text(item: &Value) -> Option<&str> {
    if let Some(text) = item.as_str() {
        return Some(text);
    }
    if matches!(
        item.get("type").and_then(Value::as_str),
        Some("reasoning" | "reasoning_text" | "reasoning_summary_text")
    ) {
        return None;
    }
    item.get("text")
        .or_else(|| item.get("input_text"))
        .or_else(|| item.get("output_text"))
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
}

/// Returns only the new reasoning text to append for a streaming delta.
/// Providers may send either incremental fragments or cumulative snapshots.
fn reasoning_stream_delta<'a>(accumulated: &'a str, incoming: &'a str) -> Option<&'a str> {
    if incoming.is_empty() {
        return None;
    }
    if accumulated.is_empty() {
        return Some(incoming);
    }
    if let Some(suffix) = incoming.strip_prefix(accumulated) {
        return (!suffix.is_empty()).then_some(suffix);
    }
    Some(incoming)
}

fn chat_reasoning_text(value: &Value) -> Option<String> {
    if let Some(text) = value
        .get("reasoning_content")
        .or_else(|| value.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_string());
    }
    // OpenRouter and some gateways return reasoning as a `reasoning_details`
    // array of objects such as {"type": "text", "text": "..."} or plain strings.
    // Some providers also emit `reasoning_details` as a single string or object,
    // or use object shapes like {"type": "reasoning.summary", "text": "..."} /
    // {"summary": "..."}. Flatten the contiguous text so hybrid-thinking models
    // (e.g. Hy3) surface their chain-of-thought to Codex, and never silently
    // discard all reasoning when the field is not an array.
    let details: Vec<&Value> = match value.get("reasoning_details") {
        Some(Value::Array(arr)) => arr.iter().collect(),
        Some(Value::String(s)) => return Some(s.clone()),
        Some(Value::Object(obj)) => {
            if let Some(text) = obj
                .get("text")
                .or_else(|| obj.get("summary"))
                .or_else(|| obj.get("reasoning"))
                .and_then(Value::as_str)
            {
                return Some(text.to_string());
            }
            return None;
        }
        _ => return None,
    };
    let mut combined = String::new();
    for item in details {
        match item {
            Value::String(text) => combined.push_str(text),
            Value::Object(obj) => {
                if let Some(text) = obj
                    .get("text")
                    .or_else(|| obj.get("summary"))
                    .or_else(|| obj.get("reasoning"))
                    .and_then(Value::as_str)
                {
                    combined.push_str(text);
                }
            }
            _ => {}
        }
    }
    if combined.is_empty() {
        None
    } else {
        Some(combined)
    }
}

pub(crate) fn chat_usage_to_responses_usage(usage: Option<&Value>) -> Value {
    let Some(usage) = usage else {
        return Value::Null;
    };
    if usage.is_null() {
        return Value::Null;
    }

    let input_tokens =
        token_count(usage, &["input_tokens"]).or_else(|| token_count(usage, &["prompt_tokens"]));
    let output_tokens = token_count(usage, &["output_tokens"])
        .or_else(|| token_count(usage, &["completion_tokens"]));
    let total_tokens = token_count(usage, &["total_tokens"]).or_else(|| {
        Some(
            input_tokens
                .unwrap_or_default()
                .saturating_add(output_tokens.unwrap_or_default()),
        )
    });
    let cached_tokens = token_count(usage, &["input_tokens_details", "cached_tokens"])
        .or_else(|| token_count(usage, &["prompt_tokens_details", "cached_tokens"]))
        .or_else(|| token_count(usage, &["cached_tokens"]))
        .or_else(|| token_count(usage, &["prompt_cache_hit_tokens"]))
        .unwrap_or_default();
    let reasoning_tokens = token_count(usage, &["output_tokens_details", "reasoning_tokens"])
        .or_else(|| token_count(usage, &["completion_tokens_details", "reasoning_tokens"]))
        .unwrap_or_default();

    json!({
        "input_tokens": input_tokens.unwrap_or_default(),
        "input_tokens_details": if cached_tokens > 0 {
            json!({"cached_tokens": cached_tokens})
        } else {
            Value::Null
        },
        "output_tokens": output_tokens.unwrap_or_default(),
        "output_tokens_details": if reasoning_tokens > 0 {
            json!({"reasoning_tokens": reasoning_tokens})
        } else {
            Value::Null
        },
        "total_tokens": total_tokens.unwrap_or_default()
    })
}

fn token_count(value: &Value, path: &[&str]) -> Option<i64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_i64()
        .or_else(|| current.as_u64().and_then(|value| i64::try_from(value).ok()))
}

pub(crate) fn chat_completion_payload(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

pub(crate) fn chat_stream_debug_summary(payload: &Value, events: &[String]) -> Option<Value> {
    let choices = payload
        .get("choices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut reasoning_content_chars = 0;
    let mut reasoning_chars = 0;
    let mut reasoning_details_count = 0;
    let mut reasoning_details_shapes = BTreeSet::new();
    let mut content_chars = 0;
    let mut tool_call_delta_count = 0;
    let mut fields = BTreeSet::new();

    for choice in choices {
        let delta = choice.get("delta").unwrap_or(&Value::Null);
        if let Some(text) = delta.get("reasoning_content").and_then(Value::as_str) {
            reasoning_content_chars += text.chars().count();
            fields.insert("reasoning_content");
        }
        if let Some(text) = delta.get("reasoning").and_then(Value::as_str) {
            reasoning_chars += text.chars().count();
            fields.insert("reasoning");
        }
        if let Some(details) = delta.get("reasoning_details") {
            fields.insert("reasoning_details");
            match details {
                Value::Array(items) => {
                    reasoning_details_count += items.len();
                    for item in items {
                        reasoning_details_shapes.insert(value_shape(item));
                    }
                }
                value => {
                    reasoning_details_count += 1;
                    reasoning_details_shapes.insert(value_shape(value));
                }
            }
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str) {
            content_chars += text.chars().count();
            fields.insert("content");
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            tool_call_delta_count += calls.len();
            fields.insert("tool_calls");
        }
    }

    let emitted_reasoning_events = events
        .iter()
        .filter(|event| event.contains("response.reasoning_summary_text.delta"))
        .count();
    let emitted_text_events = events
        .iter()
        .filter(|event| event.contains("response.output_text.delta"))
        .count();

    let has_signal = reasoning_content_chars > 0
        || reasoning_chars > 0
        || reasoning_details_count > 0
        || content_chars > 0
        || tool_call_delta_count > 0
        || emitted_reasoning_events > 0
        || emitted_text_events > 0;
    has_signal.then(|| {
        json!({
            "upstream_fields": fields.into_iter().collect::<Vec<_>>(),
            "reasoning_content_chars": reasoning_content_chars,
            "reasoning_chars": reasoning_chars,
            "reasoning_details_count": reasoning_details_count,
            "reasoning_details_shapes": reasoning_details_shapes.into_iter().collect::<Vec<_>>(),
            "content_chars": content_chars,
            "tool_call_delta_count": tool_call_delta_count,
            "emitted_reasoning_delta_events": emitted_reasoning_events,
            "emitted_output_text_delta_events": emitted_text_events
        })
    })
}

fn value_shape(value: &Value) -> String {
    match value {
        Value::Object(object) => {
            let keys = object.keys().cloned().collect::<Vec<_>>().join(",");
            format!("object:{keys}")
        }
        Value::Array(items) => format!("array:{}", items.len()),
        Value::String(_) => "string".to_string(),
        Value::Number(_) => "number".to_string(),
        Value::Bool(_) => "bool".to_string(),
        Value::Null => "null".to_string(),
    }
}

pub(crate) fn native_stream_debug_summary(value: &Value) -> Option<Value> {
    let event_type = value.get("type").and_then(Value::as_str);
    let delta = value.get("delta").and_then(Value::as_str).unwrap_or("");
    let item_type = value
        .get("item")
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str);
    let has_reasoning_delta = matches!(
        event_type,
        Some("response.reasoning_text.delta" | "response.reasoning_summary_text.delta")
    );
    let has_output_delta = matches!(event_type, Some("response.output_text.delta"));
    let has_tool_item = is_function_call_type(item_type) || is_custom_tool_call_type(item_type);

    (has_reasoning_delta || has_output_delta || has_tool_item).then(|| {
        json!({
            "event_type": event_type,
            "item_type": item_type,
            "reasoning_delta_chars": if has_reasoning_delta { delta.chars().count() } else { 0 },
            "output_text_delta_chars": if has_output_delta { delta.chars().count() } else { 0 },
            "has_tool_item": has_tool_item
        })
    })
}

fn custom_tool_input(arguments: &str) -> String {
    match serde_json::from_str::<Value>(arguments) {
        // The model returned a JSON-encoded string for the patch input.
        Ok(Value::String(s)) => s,
        Ok(Value::Object(obj)) => {
            if let Some(input) = obj.get("input").and_then(Value::as_str) {
                return input.to_string();
            }
            arguments.to_string()
        }
        // Non-JSON or an unexpected JSON shape: pass the raw arguments through
        // as the input so the failure is visible rather than silently dropped.
        _ => arguments.to_string(),
    }
}

pub(crate) fn morph_native_sse_frame(
    frame: &str,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
) -> String {
    let mut event_lines = Vec::new();
    let mut data_lines = Vec::new();

    let normalized = frame.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.lines() {
        if let Some(data) = line.strip_prefix("data:") {
            data_lines.push(data.trim_start());
        } else {
            event_lines.push(line);
        }
    }

    if data_lines.is_empty() {
        return format!("{frame}\n\n");
    }

    let data = data_lines.join("\n");
    if data == "[DONE]" {
        return format!("{frame}\n\n");
    }

    let Ok(mut value) = serde_json::from_str::<Value>(&data) else {
        return format!("{frame}\n\n");
    };
    morph_native_response_value(
        &mut value,
        custom_tool_names,
        namespace_helpers,
        tool_policy,
    );

    let mut out = String::new();
    for line in event_lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("data: ");
    out.push_str(&value.to_string());
    out.push_str("\n\n");
    out
}

pub(crate) fn morph_native_response_value(
    value: &mut Value,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
) {
    if custom_tool_names.is_empty() && namespace_helpers.is_empty() && !tool_policy.enabled {
        return;
    }

    if let Some(item) = value.get_mut("item") {
        morph_native_item(item, custom_tool_names, namespace_helpers, tool_policy);
    }
    if let Some(response) = value.get_mut("response")
        && let Some(output) = response.get_mut("output").and_then(Value::as_array_mut)
    {
        for item in output {
            morph_native_item(item, custom_tool_names, namespace_helpers, tool_policy);
        }
    }
    if let Some(output) = value.get_mut("output").and_then(Value::as_array_mut) {
        for item in output {
            morph_native_item(item, custom_tool_names, namespace_helpers, tool_policy);
        }
    }
}

fn morph_native_item(
    item: &mut Value,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
) {
    let Some(name) = item.get("name").and_then(Value::as_str) else {
        return;
    };
    if !is_function_call_type(item.get("type").and_then(Value::as_str)) {
        return;
    }
    let arguments = item
        .get("arguments")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let source_is_custom = custom_tool_names.contains(name)
        && !namespace_helpers.is_expanded_namespace_function_alias(name);
    let rewritten = rewrite_provider_call(namespace_helpers, name, arguments, source_is_custom);
    let runtime_name = rewritten_runtime_name(&rewritten);
    apply_classified_call_to_native_item(
        item,
        classify_rewritten_call(
            &runtime_name,
            &rewritten.arguments,
            source_is_custom,
            tool_policy,
        ),
    );
    apply_rewritten_call_metadata(item, &rewritten);
}

fn tool_call_item(
    name: &str,
    arguments: &str,
    call_id: &str,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
) -> Value {
    let source_is_custom = custom_tool_names.contains(name)
        && !namespace_helpers.is_expanded_namespace_function_alias(name);
    let rewritten = rewrite_provider_call(namespace_helpers, name, arguments, source_is_custom);
    let runtime_name = rewritten_runtime_name(&rewritten);
    let mut item = classified_tool_call_item(
        &runtime_name,
        &rewritten.arguments,
        call_id,
        source_is_custom,
        tool_policy,
    );
    apply_rewritten_call_metadata(&mut item, &rewritten);
    item
}

fn rewrite_provider_call(
    namespace_helpers: &NamespaceHelpers,
    name: &str,
    arguments: &str,
    source_is_custom: bool,
) -> RewrittenCall {
    if source_is_custom {
        return RewrittenCall {
            name: name.to_string(),
            namespace: None,
            arguments: arguments.to_string(),
            plaintext_encrypted_arguments: false,
        };
    }
    namespace_helpers.rewrite_response_call(name, arguments)
}

fn rewritten_runtime_name(call: &RewrittenCall) -> String {
    call.namespace.as_ref().map_or_else(
        || call.name.clone(),
        |namespace| format!("{namespace}.{}", call.name),
    )
}

fn apply_rewritten_call_metadata(item: &mut Value, call: &RewrittenCall) {
    let item_type = item.get("type").and_then(Value::as_str);
    let is_function = is_function_call_type(item_type);
    let is_custom = is_custom_tool_call_type(item_type);
    if matches!((is_function, is_custom), (false, false)) {
        return;
    }
    let Some(map) = item.as_object_mut() else {
        return;
    };
    map.insert("name".to_string(), json!(call.name));
    if let Some(namespace) = &call.namespace {
        map.insert("namespace".to_string(), json!(namespace));
    } else {
        map.remove("namespace");
    }
    if call.plaintext_encrypted_arguments && is_function {
        map.insert("encrypted_function_args".to_string(), json!([]));
    }
}

enum ClassifiedCall {
    Custom { name: String, input: String },
    Function { name: String, arguments: String },
    Blocked { name: String, reason: String },
}

fn classify_rewritten_call(
    name: &str,
    arguments: &str,
    source_is_custom: bool,
    tool_policy: &ToolPolicyConfig,
) -> ClassifiedCall {
    if source_is_custom {
        return ClassifiedCall::Custom {
            name: name.to_string(),
            input: custom_tool_input(arguments),
        };
    }
    match apply_tool_policy_to_function_call(name, arguments, tool_policy) {
        Ok((arguments, _decision)) => ClassifiedCall::Function {
            name: name.to_string(),
            arguments,
        },
        Err(decision) => ClassifiedCall::Blocked {
            name: name.to_string(),
            reason: decision.reason,
        },
    }
}

fn apply_classified_call_to_native_item(item: &mut Value, classified: ClassifiedCall) {
    match classified {
        ClassifiedCall::Custom { name, input } => {
            if let Some(map) = item.as_object_mut() {
                map.insert(
                    "type".to_string(),
                    Value::String("custom_tool_call".to_string()),
                );
                map.insert("name".to_string(), json!(name));
                map.remove("arguments");
                map.insert("input".to_string(), Value::String(input));
            }
        }
        ClassifiedCall::Function { name, arguments } => {
            if let Some(map) = item.as_object_mut() {
                // Normalize backend `tool_call` into the Responses item type Codex expects.
                map.insert(
                    "type".to_string(),
                    Value::String("function_call".to_string()),
                );
                map.insert("name".to_string(), json!(name));
                map.insert("arguments".to_string(), json!(arguments));
            }
        }
        ClassifiedCall::Blocked { name, reason } => {
            *item = blocked_tool_call_message(&name, &reason);
        }
    }
}

fn classified_tool_call_item(
    name: &str,
    arguments: &str,
    call_id: &str,
    source_is_custom: bool,
    tool_policy: &ToolPolicyConfig,
) -> Value {
    match classify_rewritten_call(name, arguments, source_is_custom, tool_policy) {
        ClassifiedCall::Custom { name, input } => json!({
            "id": generated_id("ctc"),
            "type": "custom_tool_call",
            "name": name,
            "input": input,
            "call_id": call_id
        }),
        ClassifiedCall::Function { name, arguments } => json!({
            "id": generated_id("fc"),
            "type": "function_call",
            "name": name,
            "arguments": arguments,
            "call_id": call_id
        }),
        ClassifiedCall::Blocked { name, reason } => blocked_tool_call_message(&name, &reason),
    }
}

fn blocked_tool_call_message(name: &str, reason: &str) -> Value {
    json!({
        "id": generated_id("msg"),
        "type": "message",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": format!("Codex Warp blocked tool call `{name}`: {reason}")
        }]
    })
}

fn sse(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

pub(crate) fn sse_data(frame: &str) -> Option<String> {
    // Event streams permit CRLF, LF, or CR line endings.
    let normalized = frame.replace("\r\n", "\n").replace('\r', "\n");
    let data = normalized
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(str::trim_start)
        .collect::<Vec<_>>()
        .join("\n");
    (!data.is_empty()).then_some(data)
}

pub(crate) fn next_sse_frame_bytes(buffer: &[u8]) -> Option<(usize, usize)> {
    fn find_bytes(buffer: &[u8], needle: &[u8]) -> Option<usize> {
        buffer
            .windows(needle.len())
            .position(|window| window == needle)
    }

    [
        find_bytes(buffer, b"\n\n").map(|index| (index, 2)),
        find_bytes(buffer, b"\r\n\r\n").map(|index| (index, 4)),
        find_bytes(buffer, b"\r\r").map(|index| (index, 2)),
    ]
    .into_iter()
    .flatten()
    .min_by_key(|(index, _)| *index)
}

#[cfg(test)]
#[path = "response_codec_tests.rs"]
mod tests;
