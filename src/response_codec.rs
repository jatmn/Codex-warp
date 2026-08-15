use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Mutex;
use std::sync::OnceLock;

use async_stream::stream;
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

use crate::config::ContinueGuardConfig;
use crate::config::ContinueGuardMode;
use crate::config::ToolPolicyConfig;
use crate::debug_log::DebugLog;
use crate::debug_log::text_fingerprint;
use crate::ids::generated_id;
use crate::namespace_helpers::NamespaceHelpers;
use crate::namespace_helpers::is_custom_tool_call_type;
use crate::namespace_helpers::is_function_call_type;
use crate::store::UsageRecorder;
use crate::tool_policy::apply_tool_policy_to_function_call;

const REASONING_DISPLAY_HEADER: &str = "**Reasoning**\n\n";

/// Maximum number of entries allowed in the continue-guard budget map to prevent
/// unbounded memory growth during long-running proxy sessions.
const CONTINUE_GUARD_BUDGET_MAX_ENTRIES: usize = 10_000;

/// Maximum number of bytes allowed in the SSE frame buffer before treating the
/// upstream as misbehaving and returning an error.
const SSE_FRAME_BUFFER_MAX_BYTES: usize = 16 * 1024 * 1024;
const SSE_FRAME_BUFFER_EXCEEDED_MESSAGE: &str = "upstream SSE frame buffer exceeded maximum size";

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
    stream! {
        let created_event = sse("response.created", json!({
            "type": "response.created",
            "response": {"id": response_id, "object": "response", "status": "in_progress"}
        }));
        log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &created_event);
        yield Ok(Bytes::from(created_event));

        let mut state = ChatAccum::default();
        let mut pending = Vec::new();
        let mut bytes = upstream.bytes_stream();
        let mut completed = false;
        let mut response_observed = false;
        let usage_recorder = usage_recorder;

        'upstream: while let Some(chunk) = bytes.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(err) => {
                    yield Ok(Bytes::from(chat_failed_event(&response_id, err.to_string())));
                    return;
                }
            };
            pending.extend_from_slice(&chunk);
            if pending.len() > SSE_FRAME_BUFFER_MAX_BYTES {
                yield Ok(Bytes::from(chat_failed_event(&response_id, "upstream SSE frame buffer exceeded maximum size")));
                return;
            }

            while let Some((frame_end, delimiter_len)) = next_sse_frame_bytes(&pending) {
                let frame = pending[..frame_end].to_vec();
                pending.drain(..frame_end + delimiter_len);
                let Ok(frame) = String::from_utf8(frame) else {
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
                        yield Ok(Bytes::from(chat_failed_event(
                            &response_id,
                            "upstream chat stream contained invalid JSON",
                        )));
                        return;
                    }
                };
                let payload = chat_completion_payload(&value);
                if let Some(message) = upstream_error_message(payload) {
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
            log_downstream_sse_frame(&debug_log, &request_log_id, "open_ai_chat", &failed);
            yield Ok(Bytes::from(failed));
            return;
        }

        if let Some(recorder) = &usage_recorder {
            recorder.record_completed(state.usage.as_ref());
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
    stream! {
        let mut pending = Vec::new();
        let mut bytes = upstream.bytes_stream();
        let mut pending_usage: Option<Value> = None;
        let mut terminal_received = false;
        let mut usage_recorder = usage_recorder;
        let mut response_id = None;

        'upstream: while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(std::io::Error::other)?;
            pending.extend_from_slice(&chunk);
            if pending.len() > SSE_FRAME_BUFFER_MAX_BYTES {
                yield Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    SSE_FRAME_BUFFER_EXCEEDED_MESSAGE,
                ));
                return;
            }
            while let Some((frame_end, delimiter_len)) = next_sse_frame_bytes(&pending) {
                let frame = pending[..frame_end].to_vec();
                pending.drain(..frame_end + delimiter_len);
                let frame = match String::from_utf8(frame) {
                    Ok(frame) => frame,
                    Err(err) => {
                        yield Err(std::io::Error::new(std::io::ErrorKind::InvalidData, err));
                        return;
                    }
                };
                response_id = native_sse_response_id(&frame).or(response_id);
                if let Some(message) = native_sse_error_message(&frame) {
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
                if terminal == Some(NativeSseTerminal::Completed) {
                    if let Some(recorder) = usage_recorder.take() {
                        recorder.record_completed(pending_usage.as_ref());
                    }
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
                log_downstream_sse_frame(&debug_log, &request_log_id, "responses", &morphed);
                yield Ok(Bytes::from(morphed));
                if terminal.is_some() {
                    break 'upstream;
                }
            }
        }

        if !terminal_received {
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
    NonSuccess,
}

/// Classify response-level terminal events independently from success. Native
/// streams can finish successfully, fail, be cancelled, or be incomplete; all
/// of those outcomes make EOF expected, but only a completed response records
/// successful usage analytics.
fn native_sse_terminal(frame: &str) -> Option<NativeSseTerminal> {
    let data = sse_data(frame)?;
    let value = serde_json::from_str::<Value>(&data).ok()?;
    match value.get("type").and_then(Value::as_str)? {
        "response.completed" => {
            let response = value.get("response")?.as_object()?;
            match response.get("status").and_then(Value::as_str) {
                None | Some("completed") => Some(NativeSseTerminal::Completed),
                _ => Some(NativeSseTerminal::NonSuccess),
            }
        }
        "response.failed" | "response.cancelled" | "response.incomplete" => {
            Some(NativeSseTerminal::NonSuccess)
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
        .and_then(|value| {
            value
                .get("usage")
                .or_else(|| {
                    value
                        .get("response")
                        .and_then(|response| response.get("usage"))
                })
                .cloned()
        })
        .unwrap_or(Value::Null)
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
    let usage = value.get("usage").or_else(|| {
        value
            .get("response")
            .and_then(|response| response.get("usage"))
    });
    if let Some(usage) = usage
        && !usage.is_null()
    {
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
    tool_calls: Vec<ToolCallAccum>,
    usage: Option<Value>,
    finish_reason: Option<String>,
}

#[derive(Default, Clone)]
pub(crate) struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

impl ChatAccum {
    pub(crate) fn apply_chat_chunk(&mut self, chunk: &Value) -> Vec<String> {
        let mut events = Vec::new();
        if let Some(usage) = chunk.get("usage") {
            self.usage = Some(chat_usage_to_responses_usage(Some(usage)));
        }
        let choices = chunk
            .get("choices")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for choice in choices {
            let delta = choice.get("delta").unwrap_or(&Value::Null);
            if let Some(finish_reason) = choice.get("finish_reason").and_then(Value::as_str) {
                self.finish_reason = Some(finish_reason.to_string());
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
                if !self.reasoning_display_header_emitted {
                    self.reasoning_display_header_emitted = true;
                    if !incoming.trim_start().starts_with("**") {
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
                self.reasoning_text.push_str(&reasoning);
                events.push(sse(
                    "response.reasoning_summary_text.delta",
                    json!({
                        "type": "response.reasoning_summary_text.delta",
                        "item_id": self.reasoning_item_id.as_deref().unwrap_or(""),
                        "summary_index": 0,
                        "delta": reasoning.as_str()
                    }),
                ));
            }

            if let Some(content) = delta.get("content").and_then(Value::as_str)
                && !content.is_empty()
            {
                if self.message_item_id.is_none() {
                    let item_id = generated_id("msg");
                    self.message_item_id = Some(item_id.clone());
                    events.push(sse(
                        "response.output_item.added",
                        json!({
                            "type": "response.output_item.added",
                            "item": {
                                "id": item_id,
                                "type": "message",
                                "role": "assistant",
                                "content": []
                            }
                        }),
                    ));
                }
                self.text.push_str(content);
                events.push(sse(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta",
                        "delta": content
                    }),
                ));
            }

            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for call in calls {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                    if self.tool_calls.len() <= index {
                        self.tool_calls
                            .resize_with(index + 1, ToolCallAccum::default);
                    }
                    let acc = &mut self.tool_calls[index];
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
        &self,
        response_id: &str,
        custom_tool_names: &BTreeSet<String>,
        namespace_helpers: &NamespaceHelpers,
        tool_policy: &ToolPolicyConfig,
        continue_guard: Option<(&DebugLog, &str, &ContinueGuardState)>,
    ) -> Vec<String> {
        let mut events = Vec::new();
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

        for call in &self.tool_calls {
            if call.name.is_empty() {
                continue;
            }
            let call_id = if call.id.is_empty() {
                generated_id("call")
            } else {
                call.id.clone()
            };
            let item = tool_call_item(
                &call.name,
                &call.arguments,
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
        if self
            .active_plan
            .as_ref()
            .is_some_and(|plan| !plan.has_open_items())
            && !self.progress
        {
            return ContinueGuardDecision::none("plan_completed");
        }
        if !looks_like_mid_task_stop(&accum.text) {
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
    plans.into_iter().filter_map(parse_plan_summary).last()
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
/// and let text-only pause loops run past `max_followups`.
fn request_shows_tool_progress(request: &Value) -> bool {
    if let Some(input) = request.get("input").and_then(Value::as_array) {
        return input.last().is_some_and(item_shows_tool_progress);
    }
    // Defensive chat-completions shape (some callers may pass a converted body):
    // tool results or pending assistant tool calls at the end of `messages`.
    request
        .get("messages")
        .and_then(Value::as_array)
        .and_then(|messages| messages.last())
        .is_some_and(chat_message_shows_tool_progress)
}

fn item_shows_tool_progress(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call_output" | "custom_tool_call_output") => true,
        Some("function_call" | "tool_call" | "custom_tool_call") => !item_is_update_plan(item),
        _ => false,
    }
}

fn item_is_update_plan(item: &Value) -> bool {
    item.get("name").and_then(Value::as_str) == Some("update_plan")
        || item
            .get("function")
            .and_then(|function| function.get("name"))
            .and_then(Value::as_str)
            == Some("update_plan")
}

fn chat_message_shows_tool_progress(message: &Value) -> bool {
    if message.get("role").and_then(Value::as_str) == Some("tool") {
        return true;
    }
    message
        .get("tool_calls")
        .and_then(Value::as_array)
        .is_some_and(|calls| calls.iter().any(|call| !item_is_update_plan(call)))
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
    // 1. Closers that contain work-like substrings ("let me know").
    // 2. First-person / let-me prefixes: after stripping adverbs and nested
    //    prefixes, the next action is work when it is a known work verb, or
    //    an unlisted verb with a non-hand-off object ("I'll clone the repo",
    //    "I'll add tests"). Wrap-up verbs and hand-off complements do not
    //    count ("Now let me summarize", "I'll update you", "look at your PR").
    // 3. Wrap-up / hand-off phrasing. This loses to a prefix+work-action pair
    //    so "Thanks to the rebase. Now let me verify" still continues.
    // 4. Dangling `:`/`...` only when the last sentence still talks about
    //    remaining work. Bare delivery colons ("Here is the final report:")
    //    are not pauses.
    if contains_overlapping_closing_phrase(&normalized) {
        return false;
    }
    if contains_work_intent(&normalized) {
        return true;
    }
    if contains_wrap_up_closing_phrase(&normalized) {
        return false;
    }
    dangling_punctuation_with_remaining_work(&normalized)
}

fn contains_work_intent(normalized: &str) -> bool {
    const PREFIXES: [&str; 9] = [
        "let me ",
        "i'll ",
        "i will ",
        "i still need to ",
        "i need to ",
        "i'm going to ",
        "i should ",
        "then ",
        "next ",
    ];
    PREFIXES.iter().any(|prefix| {
        let mut start = 0;
        while let Some(idx) = normalized[start..].find(prefix) {
            let after_prefix = &normalized[start + idx + prefix.len()..];
            if remainder_is_work_action(strip_intent_fillers(after_prefix)) {
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
    let complement = strip_leading_prepositions(action_complement(rest));
    if complement_is_hand_off(complement) {
        return false;
    }
    // Known work verbs may stand alone ("Let me check."). Unlisted verbs are
    // work only when they act on an object ("I'll clone the repo", "I'll add
    // tests"), not because the token is long ("I'll think about this").
    remainder_starts_with_work_verb(rest) || !complement.is_empty()
}

fn action_complement(rest: &str) -> &str {
    let end = rest
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(rest.len());
    rest[end..].trim_start()
}

fn strip_leading_prepositions(mut complement: &str) -> &str {
    loop {
        let Some(next) = [
            "at ", "in ", "on ", "into ", "from ", "with ", "for ", "of ",
        ]
        .iter()
        .find_map(|preposition| complement.strip_prefix(preposition)) else {
            return complement;
        };
        complement = next;
    }
}

fn complement_is_hand_off(complement: &str) -> bool {
    let end = complement
        .find(|c: char| !c.is_ascii_alphabetic())
        .unwrap_or(complement.len());
    matches!(
        &complement[..end],
        "you" | "your" | "about" | "here" | "if" | "whether" | "when"
    )
}

fn remainder_starts_with_wrap_up_action(rest: &str) -> bool {
    [
        "summarize",
        "stop",
        "leave",
        "wrap",
        "explain",
        "tell",
        "know",
        "wait",
        "pause",
        "recap",
        "conclude",
        "help",
        "stay",
        "remain",
        "see",
        "think",
        "note",
        "rest",
    ]
    .iter()
    .any(|stem| token_starts_with_stem(rest, stem))
}

fn remainder_starts_with_work_verb(rest: &str) -> bool {
    const STEMS: [&str; 34] = [
        "check", "inspect", "look", "read", "write", "run", "verify", "open", "search", "audit",
        "push", "apply", "test", "fix", "review", "examine", "fetch", "pull", "grep", "list",
        "continue", "start", "compare", "confirm", "dump", "patch", "edit", "find", "scan",
        "rebase", "commit", "merge", "build", "checkout",
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
        .rsplit(|c| matches!(c, '.' | '!' | '?' | ';'))
        .next()
        .unwrap_or(normalized)
        .trim();
    [
        "pending",
        "still need",
        "still have",
        "next step",
        "remaining",
        "after that",
        "not yet",
        "to do",
        "follow up",
        "follow-up",
    ]
    .iter()
    .any(|cue| last_sentence.contains(cue))
}

fn contains_overlapping_closing_phrase(normalized: &str) -> bool {
    ["no actionable issues", "let me know"]
        .iter()
        .any(|marker| normalized.contains(marker))
}

/// Wrap-up phrasing that should not force a follow-up unless a prefix is
/// followed by a work action. Generic "let me"/"I'll"/"I need to"/"I should"
/// are not enough on their own, even with "now"/"first"/"still". Subtask
/// completion words such as "done" or "complete" are deliberately excluded:
/// mid-task text routinely says "the rebase is complete" before continuing
/// ("Now let me push...").
fn contains_wrap_up_closing_phrase(normalized: &str) -> bool {
    [
        "thank you",
        "thanks",
        "feel free",
        "that's all",
        "that is all",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

pub(crate) fn chat_json_to_responses(value: Value, custom_tool_names: &BTreeSet<String>) -> Value {
    chat_json_to_responses_with_policy(
        value,
        custom_tool_names,
        &NamespaceHelpers::default(),
        &ToolPolicyConfig::default(),
    )
}

pub(crate) fn chat_json_to_responses_with_policy(
    value: Value,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
) -> Value {
    let value = chat_completion_payload(&value);
    let response_id = value
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| generated_id("resp"));

    let mut output = Vec::new();
    if let Some(choice) = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        && let Some(message) = choice.get("message")
    {
        let reasoning = chat_reasoning_text(message);
        let content = message.get("content").and_then(Value::as_str);
        let mut message_parts = Vec::new();
        if let Some(reasoning) = reasoning
            && !reasoning.is_empty()
        {
            message_parts.push(json!({"type": "reasoning_summary_text", "text": reasoning}));
        }
        if let Some(content) = message.get("content").and_then(Value::as_str)
            && !content.is_empty()
        {
            message_parts.push(json!({"type": "output_text", "text": content}));
        }
        if !message_parts.is_empty() {
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": message_parts
            }));
        } else if content.is_some() {
            output.push(json!({
                "type": "message",
                "role": "assistant",
                "content": []
            }));
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let name = call
                    .get("function")
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("tool");
                let arguments = call
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .and_then(Value::as_str)
                    .unwrap_or("{}");
                let call_id = call.get("id").and_then(Value::as_str).unwrap_or("call");
                output.push(tool_call_item(
                    name,
                    arguments,
                    call_id,
                    custom_tool_names,
                    namespace_helpers,
                    tool_policy,
                ));
            }
        }
    }

    json!({
        "id": response_id,
        "object": "response",
        "status": "completed",
        "output": output,
        "usage": chat_usage_to_responses_usage(value.get("usage"))
    })
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
    if incoming.starts_with(accumulated) {
        let suffix = &incoming[accumulated.len()..];
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
    let (name, arguments) = namespace_helpers.rewrite_call(name, arguments);
    apply_classified_call_to_native_item(
        item,
        classify_rewritten_call(&name, &arguments, custom_tool_names, tool_policy),
    );
}

fn tool_call_item(
    name: &str,
    arguments: &str,
    call_id: &str,
    custom_tool_names: &BTreeSet<String>,
    namespace_helpers: &NamespaceHelpers,
    tool_policy: &ToolPolicyConfig,
) -> Value {
    let (name, arguments) = namespace_helpers.rewrite_call(name, arguments);
    classified_tool_call_item(&name, &arguments, call_id, custom_tool_names, tool_policy)
}

enum ClassifiedCall {
    Custom { name: String, input: String },
    Function { name: String, arguments: String },
    Blocked { name: String, reason: String },
}

fn classify_rewritten_call(
    name: &str,
    arguments: &str,
    custom_tool_names: &BTreeSet<String>,
    tool_policy: &ToolPolicyConfig,
) -> ClassifiedCall {
    if custom_tool_names.contains(name) {
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
    custom_tool_names: &BTreeSet<String>,
    tool_policy: &ToolPolicyConfig,
) -> Value {
    match classify_rewritten_call(name, arguments, custom_tool_names, tool_policy) {
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
