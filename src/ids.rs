use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn generated_id(prefix: &str) -> String {
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{prefix}_{}_{nanos:x}_{sequence:x}", std::process::id())
}

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
