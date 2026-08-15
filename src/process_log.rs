use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use serde::Serialize;
use tracing::Event;
use tracing::Subscriber;
use tracing::field::Field;
use tracing::field::Visit;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::Registry;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::Context;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;

pub(crate) const DEFAULT_PROCESS_LOG_CAPACITY: usize = 2_000;

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProcessLogEvent {
    pub ts: u64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Clone)]
pub(crate) struct ProcessLog {
    inner: Arc<Mutex<VecDeque<ProcessLogEvent>>>,
    capacity: usize,
}

impl ProcessLog {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            capacity,
        }
    }

    #[cfg(test)]
    pub(crate) fn disabled() -> Self {
        Self::new(0)
    }

    pub(crate) fn push(&self, event: ProcessLogEvent) {
        if self.capacity == 0 {
            return;
        }
        let Ok(mut events) = self.inner.lock() else {
            return;
        };
        events.push_back(ProcessLogEvent {
            ts: event.ts,
            level: event.level,
            target: event.target,
            message: crate::debug_log::redact_debug_text(&event.message),
        });
        while events.len() > self.capacity {
            events.pop_front();
        }
    }

    pub(crate) fn snapshot(
        &self,
        limit: usize,
        min_level: Option<&str>,
        query: Option<&str>,
    ) -> Vec<ProcessLogEvent> {
        let Ok(events) = self.inner.lock() else {
            return Vec::new();
        };
        let min_rank = min_level.and_then(level_rank);
        let query = query
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_ascii_lowercase());
        events
            .iter()
            .filter(|event| {
                min_rank.is_none_or(|min| level_rank(&event.level).is_some_and(|rank| rank >= min))
            })
            .filter(|event| {
                let Some(query) = query.as_deref() else {
                    return true;
                };
                event.level.to_ascii_lowercase().contains(query)
                    || event.target.to_ascii_lowercase().contains(query)
                    || event.message.to_ascii_lowercase().contains(query)
            })
            .cloned()
            .rev()
            .take(limit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
    }
}

fn level_rank(level: &str) -> Option<u8> {
    match level.to_ascii_uppercase().as_str() {
        "TRACE" => Some(0),
        "DEBUG" => Some(1),
        "INFO" => Some(2),
        "WARN" | "WARNING" => Some(3),
        "ERROR" => Some(4),
        _ => None,
    }
}

struct ProcessLogLayer {
    log: ProcessLog,
}

impl<S> Layer<S> for ProcessLogLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.log.push(ProcessLogEvent {
            ts: crate::debug_log::now_unix_ms(),
            level: event.metadata().level().to_string(),
            target: event.metadata().target().to_string(),
            message: visitor.finish(),
        });
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
    fields: Vec<(String, String)>,
}

impl MessageVisitor {
    fn finish(self) -> String {
        if self.fields.is_empty() {
            return self.message;
        }
        let extras = self
            .fields
            .into_iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(" ");
        if self.message.is_empty() {
            extras
        } else {
            format!("{} {}", self.message, extras)
        }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let formatted = format!("{value:?}");
        self.record_field(field.name(), strip_debug_quotes(&formatted));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_field(field.name(), value.to_string());
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_field(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_field(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_field(field.name(), value.to_string());
    }
}

impl MessageVisitor {
    fn record_field(&mut self, name: &str, value: String) {
        if name == "message" {
            self.message = value;
        } else {
            self.fields.push((name.to_string(), value));
        }
    }
}

fn strip_debug_quotes(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_string()
}

type TracingLayer = Box<dyn Layer<Registry> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct TracingReload {
    handle: reload::Handle<TracingLayer, Registry>,
    process_log: ProcessLog,
    /// Filter captured when tracing started (`RUST_LOG`, or `info`). Used when
    /// the live snapshot leaves `tracing_filter` unset, instead of re-reading
    /// the environment on every GET or apply.
    fallback_filter: String,
    /// Last filter successfully installed. Used to skip no-op reloads, retry
    /// after a failed reload, and report `tracing_filter_effective`. GET
    /// settings otherwise come from the live debug snapshot.
    current_filter: Arc<Mutex<String>>,
    /// Holds the reload layer when it is not installed as the global subscriber.
    /// Tests drop this slot to simulate `Handle::reload` failing after a live apply.
    #[cfg(test)]
    layer_slot: Option<Arc<Mutex<Option<reload::Layer<TracingLayer, Registry>>>>>,
}

impl TracingReload {
    fn filter_lock(&self) -> std::sync::MutexGuard<'_, String> {
        match self.current_filter.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn current_filter(&self) -> String {
        self.filter_lock().clone()
    }

    pub(crate) fn fallback_filter(&self) -> &str {
        &self.fallback_filter
    }

    /// Filter the live snapshot wants installed. Unset `tracing_filter` uses
    /// the process default captured at tracing init, not a live `RUST_LOG` read.
    pub(crate) fn wanted_filter(&self, debug: &crate::config::DebugConfig) -> String {
        tracing_filter_from_debug_or(debug, &self.fallback_filter)
    }

    pub(crate) fn reload(&self, filter: &str) -> Result<(), String> {
        let parsed = parse_tracing_filter(filter)?;
        let layer = tracing_layer(parsed, self.process_log.clone());
        self.handle
            .reload(layer)
            .map_err(|err| format!("reload tracing filter: {err}"))?;
        *self.filter_lock() = normalize_tracing_filter(filter);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn for_tests(process_log: ProcessLog) -> Self {
        Self::for_tests_with_filter(process_log, &default_tracing_filter())
    }

    #[cfg(test)]
    pub(crate) fn for_tests_with_filter(process_log: ProcessLog, filter: &str) -> Self {
        let (reload, layer) = detached_tracing_reload_with_filter(process_log, filter);
        TracingReload {
            layer_slot: Some(Arc::new(Mutex::new(Some(layer)))),
            ..reload
        }
    }

    #[cfg(test)]
    pub(crate) fn disconnect_layer(&self) {
        if let Some(slot) = &self.layer_slot {
            let _ = slot.lock().map(|mut layer| layer.take());
        }
    }
}

pub(crate) fn parse_tracing_filter(filter: &str) -> Result<EnvFilter, String> {
    let filter = normalize_tracing_filter(filter);
    EnvFilter::try_new(&filter).map_err(|err| format!("invalid tracing filter `{filter}`: {err}"))
}

pub(crate) fn normalize_tracing_filter(filter: &str) -> String {
    let trimmed = filter.trim();
    if trimmed.is_empty() {
        "info".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Process default captured only when tracing starts. Runtime resolution of
/// unset `tracing_filter` uses a pinned fallback or `info`, never this helper.
pub(crate) fn default_tracing_filter() -> String {
    std::env::var("RUST_LOG")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "info".to_string())
}

#[cfg(test)]
pub(crate) fn tracing_filter_from_debug(debug: &crate::config::DebugConfig) -> String {
    tracing_filter_from_debug_or(debug, "info")
}

pub(crate) fn tracing_filter_from_debug_or(
    debug: &crate::config::DebugConfig,
    fallback: &str,
) -> String {
    debug
        .tracing_filter
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| normalize_tracing_filter(fallback))
}

/// Settings plus the filter that live apply will actually reload.
/// Unset `tracing_filter` uses `fallback`, or `info` when that pin is absent.
/// Never re-reads `RUST_LOG`; that value is captured only when tracing starts.
pub(crate) fn validate_debug_live_config_or(
    config: &mut crate::config::DebugConfig,
    fallback: Option<&str>,
) -> Result<(), String> {
    crate::debug_log::validate_debug_settings(config)?;
    parse_tracing_filter(&tracing_filter_from_debug_or(
        config,
        fallback.unwrap_or("info"),
    ))?;
    Ok(())
}

fn tracing_layer(filter: EnvFilter, process_log: ProcessLog) -> TracingLayer {
    Box::new(
        fmt::layer()
            .and_then(ProcessLogLayer { log: process_log })
            .with_filter(filter),
    )
}

fn new_tracing_reload(
    process_log: ProcessLog,
) -> Result<(TracingReload, reload::Layer<TracingLayer, Registry>), String> {
    new_tracing_reload_with_filter(process_log, default_tracing_filter())
}

fn new_tracing_reload_with_filter(
    process_log: ProcessLog,
    filter_text: String,
) -> Result<(TracingReload, reload::Layer<TracingLayer, Registry>), String> {
    let filter_text = normalize_tracing_filter(&filter_text);
    let filter = parse_tracing_filter(&filter_text)?;
    let layer = tracing_layer(filter, process_log.clone());
    let (layer, handle) = reload::Layer::new(layer);
    Ok((
        TracingReload {
            handle,
            process_log,
            fallback_filter: filter_text.clone(),
            current_filter: Arc::new(Mutex::new(filter_text)),
            #[cfg(test)]
            layer_slot: None,
        },
        layer,
    ))
}

#[cfg(test)]
fn detached_tracing_reload_with_filter(
    process_log: ProcessLog,
    filter: &str,
) -> (TracingReload, reload::Layer<TracingLayer, Registry>) {
    new_tracing_reload_with_filter(process_log, filter.to_string()).expect("test tracing filter")
}

pub(crate) fn init_tracing(process_log: ProcessLog) -> anyhow::Result<TracingReload> {
    let (reload, layer) = new_tracing_reload(process_log).map_err(anyhow::Error::msg)?;
    tracing_subscriber::registry().with(layer).init();
    Ok(reload)
}

#[cfg(test)]
#[path = "process_log_tests.rs"]
mod tests;
