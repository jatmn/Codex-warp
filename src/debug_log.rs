use std::collections::hash_map::DefaultHasher;
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::fs::OpenOptions;
use std::hash::Hash;
use std::hash::Hasher;
use std::io::ErrorKind;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::io::Write;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::RwLock;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
#[cfg(test)]
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::SystemTime;

use serde_json::Value;
use serde_json::json;
use tracing::warn;

use crate::config::DebugConfig;

const REDACTED: &str = "[REDACTED]";
pub(crate) const DEFAULT_MAX_LOG_MB: u64 = 128;
pub(crate) const DEFAULT_MAX_LOG_AGE_DAYS: u64 = 30;
pub(crate) const DEFAULT_DEBUG_LOG_PATH: &str = "codex-warp-debug.jsonl";
pub(crate) const LOG_TAIL_READ_BYTES: u64 = 512 * 1024;
pub(crate) const DEFAULT_LOG_TAIL_LIMIT: usize = 200;
pub(crate) const MAX_LOG_TAIL_LIMIT: usize = 1_000;

#[derive(Clone)]
pub(crate) struct DebugLog {
    inner: Arc<RwLock<DebugLogInner>>,
    writer_lock: Arc<Mutex<()>>,
    #[cfg(test)]
    fail_next_commit: Arc<AtomicBool>,
}

struct DebugLogInner {
    /// Live `[debug]` snapshot. GET, debug events, and the writer all read this.
    snapshot: DebugConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct JsonlTail {
    pub path: PathBuf,
    /// Writer `enabled` captured with the path and fd.
    pub enabled: bool,
    pub file_bytes: u64,
    pub truncated: bool,
    pub missing: bool,
    pub events: Vec<Value>,
}

pub(crate) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn effective_max_log_mb(config: &DebugConfig) -> u64 {
    config
        .max_log_mb
        .filter(|mb| *mb > 0)
        .unwrap_or(DEFAULT_MAX_LOG_MB)
}

pub(crate) fn effective_max_log_age_days(config: &DebugConfig) -> u64 {
    config
        .max_log_age_days
        .filter(|days| *days > 0)
        .unwrap_or(DEFAULT_MAX_LOG_AGE_DAYS)
}

/// Canonicalize debug config at every ingestion boundary (TOML/CLI, overlays,
/// Web UI) so `enabled` and `log_path` mean the same thing to the writer and
/// the stored settings. Rotation zeros are left intact so validation can reject
/// them instead of silently substituting defaults.
pub(crate) fn normalize_debug_config(config: &mut DebugConfig) {
    if config
        .log_path
        .as_ref()
        .is_some_and(|path| path.as_os_str().is_empty())
    {
        config.log_path = None;
    }
    if config.enabled && config.log_path.is_none() {
        config.log_path = Some(PathBuf::from(DEFAULT_DEBUG_LOG_PATH));
    }
    if let Some(filter) = config.tracing_filter.as_deref()
        && filter.trim().is_empty()
    {
        config.tracing_filter = None;
    }
}

fn max_log_bytes_from_config(config: &DebugConfig) -> u64 {
    effective_max_log_mb(config).saturating_mul(1024 * 1024)
}

fn max_log_age_from_config(config: &DebugConfig) -> Duration {
    Duration::from_secs(effective_max_log_age_days(config).saturating_mul(24 * 60 * 60))
}

fn active_log_path(config: &DebugConfig) -> Option<PathBuf> {
    config.enabled.then(|| config.log_path.clone()).flatten()
}

pub(crate) fn should_rotate_log(
    file_len: u64,
    age_anchor_at: SystemTime,
    now: SystemTime,
    max_bytes: u64,
    max_age: Duration,
) -> bool {
    let too_large = file_len >= max_bytes;
    let too_old = now
        .duration_since(age_anchor_at)
        .is_ok_and(|age| age >= max_age);
    too_large || too_old
}

pub(crate) fn log_age_anchor(metadata: &fs::Metadata) -> std::io::Result<SystemTime> {
    metadata.created().or_else(|_| metadata.modified())
}

fn rotation_backup_path(path: &Path) -> PathBuf {
    let mut backup: OsString = path.as_os_str().to_owned();
    backup.push(".1");
    PathBuf::from(backup)
}

fn rotation_staging_path(path: &Path) -> PathBuf {
    let mut staging: OsString = path.as_os_str().to_owned();
    staging.push(".rotating");
    PathBuf::from(staging)
}

fn rotation_pending_backup_path(path: &Path) -> PathBuf {
    let mut pending: OsString = path.as_os_str().to_owned();
    pending.push(".1.new");
    PathBuf::from(pending)
}

fn rotation_retired_backup_path(path: &Path) -> PathBuf {
    let mut retired: OsString = path.as_os_str().to_owned();
    retired.push(".1.old");
    PathBuf::from(retired)
}

fn restore_staged_log(path: &Path, staging: &Path) {
    if staging.exists()
        && !path.exists()
        && let Err(err) = fs::rename(staging, path)
    {
        warn!(
            "failed to restore debug log {} from staging {}: {err}",
            path.display(),
            staging.display()
        );
    }
}

fn restore_pending_to_staging(staging: &Path, pending: &Path) {
    if pending.exists()
        && !staging.exists()
        && let Err(err) = fs::rename(pending, staging)
    {
        warn!(
            "failed to restore debug log staging {} from pending {}: {err}",
            staging.display(),
            pending.display()
        );
    }
}

fn rollback_failed_backup_promotion(backup: &Path, retired: &Path, staging: &Path, pending: &Path) {
    if retired.exists()
        && !backup.exists()
        && let Err(err) = fs::rename(retired, backup)
    {
        warn!(
            "failed to restore debug log backup {} from retired {}: {err}",
            backup.display(),
            retired.display()
        );
    }
    restore_pending_to_staging(staging, pending);
}

/// Finish or roll back a promotion interrupted after `staging → .1.new` and/or
/// `backup → .1.old`.
fn recover_pending_backup_promotion(path: &Path) -> std::io::Result<()> {
    let backup = rotation_backup_path(path);
    let pending = rotation_pending_backup_path(path);
    let retired = rotation_retired_backup_path(path);

    if pending.exists() {
        if backup.exists() && !retired.exists() {
            fs::rename(&backup, &retired)?;
            fs::rename(&pending, &backup)?;
            let _ = fs::remove_file(&retired);
        } else if !backup.exists() && retired.exists() {
            fs::rename(&pending, &backup)?;
            let _ = fs::remove_file(&retired);
        } else if !backup.exists() {
            fs::rename(&pending, &backup)?;
        } else {
            let _ = fs::remove_file(&retired);
            fs::rename(&pending, &backup)?;
        }
    } else if retired.exists() && !backup.exists() {
        fs::rename(&retired, &backup)?;
    } else if retired.exists() {
        let _ = fs::remove_file(&retired);
    }

    Ok(())
}

/// Move `staging` into `{path}.1` without deleting the prior backup until the
/// staged segment is committed at the backup path.
pub(crate) fn promote_staging_to_backup(
    staging: &Path,
    backup: &Path,
    path: &Path,
) -> std::io::Result<()> {
    let pending = rotation_pending_backup_path(path);
    let retired = rotation_retired_backup_path(path);

    recover_pending_backup_promotion(path)?;

    fs::rename(staging, &pending)?;

    if backup.exists() {
        match fs::rename(backup, &retired) {
            Ok(()) => {}
            Err(err) => {
                restore_pending_to_staging(staging, &pending);
                return Err(err);
            }
        }
    }

    match fs::rename(&pending, backup) {
        Ok(()) => {
            if retired.exists() {
                let _ = fs::remove_file(&retired);
            }
            Ok(())
        }
        Err(err) => {
            rollback_failed_backup_promotion(backup, &retired, staging, &pending);
            Err(err)
        }
    }
}

/// Recover from a crash or kill that interrupted staging rename.
pub(crate) fn recover_interrupted_rotation(path: &Path) -> std::io::Result<()> {
    recover_pending_backup_promotion(path)?;

    let backup = rotation_backup_path(path);
    let staging = rotation_staging_path(path);
    if !staging.exists() {
        return Ok(());
    }

    if path.exists() {
        return promote_staging_to_backup(&staging, &backup, path);
    }

    if backup.exists() {
        restore_staged_log(path, &staging);
        return Ok(());
    }

    promote_staging_to_backup(&staging, &backup, path)
}

/// Move `path` to `{path}.1` via a staging file so the active log and prior
/// backup are not lost if promotion fails.
fn rotate_log_to_backup(path: &Path, backup: &Path, staging: &Path) -> std::io::Result<()> {
    recover_interrupted_rotation(path)?;
    if staging.exists() {
        return Err(std::io::Error::other(format!(
            "debug log staging file {} still exists after recovery",
            staging.display()
        )));
    }
    fs::rename(path, staging)?;
    match promote_staging_to_backup(staging, backup, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            restore_staged_log(path, staging);
            Err(err)
        }
    }
}

/// Rotate `path` to `{path}.1` when it exceeds size or age limits.
///
/// Note: this is serialized by the per-instance writer lock in `DebugLog::log`
/// and `DebugLog::commit_inner`, but multiple Warp processes sharing the same
/// `log_path` can still race. In that situation the backup may be overwritten
/// or removed unexpectedly; use a distinct `log_path` per instance.
fn maybe_rotate_log(path: &Path, max_bytes: u64, max_age: Duration) -> std::io::Result<()> {
    recover_interrupted_rotation(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    if metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("debug log path {} must not be a symlink", path.display()),
        ));
    }
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("debug log path {} is not a file", path.display()),
        ));
    }
    let age_anchor_at = log_age_anchor(&metadata)?;
    if !should_rotate_log(
        metadata.len(),
        age_anchor_at,
        SystemTime::now(),
        max_bytes,
        max_age,
    ) {
        return Ok(());
    }
    let backup = rotation_backup_path(path);
    let staging = rotation_staging_path(path);
    rotate_log_to_backup(path, &backup, &staging)
}

impl DebugLog {
    pub(crate) fn disabled() -> Self {
        Self::from_inner(DebugLogInner {
            snapshot: DebugConfig::default(),
        })
    }

    pub(crate) fn new(config: &DebugConfig) -> Result<Self, String> {
        let log = Self::disabled();
        log.apply_config(config)?;
        Ok(log)
    }

    fn from_inner(inner: DebugLogInner) -> Self {
        Self {
            inner: Arc::new(RwLock::new(inner)),
            writer_lock: Arc::new(Mutex::new(())),
            #[cfg(test)]
            fail_next_commit: Arc::new(AtomicBool::new(false)),
        }
    }

    #[cfg(test)]
    pub(crate) fn fail_next_commit(&self) {
        self.fail_next_commit.store(true, Ordering::SeqCst);
    }

    pub(crate) fn live_snapshot(&self) -> DebugConfig {
        self.read_inner().snapshot.clone()
    }

    pub(crate) fn include_bodies(&self) -> bool {
        self.read_inner().snapshot.include_bodies
    }

    pub(crate) fn include_stream_bodies(&self) -> bool {
        self.read_inner().snapshot.include_stream_bodies
    }

    #[cfg(test)]
    pub(crate) fn current_path(&self) -> Option<PathBuf> {
        active_log_path(&self.read_inner().snapshot)
    }

    pub(crate) fn apply_config(&self, config: &DebugConfig) -> Result<(), String> {
        let mut config = config.clone();
        normalize_debug_config(&mut config);
        validate_debug_settings(&mut config)?;
        self.commit_inner(&config)?;
        Ok(())
    }

    fn commit_inner(&self, config: &DebugConfig) -> Result<Option<PathBuf>, String> {
        #[cfg(test)]
        if self.fail_next_commit.swap(false, Ordering::SeqCst) {
            return Err("injected debug log commit failure".to_string());
        }
        let path = active_log_path(config);
        let Ok(_guard) = self.writer_lock.lock() else {
            return Err("failed to lock debug log writer while applying config".to_string());
        };
        if let Some(path) = path.as_ref() {
            open_debug_log(path, true)
                .map_err(|err| format!("cannot open debug log {}: {err}", path.display()))?;
        }
        {
            let mut inner = self.write_inner();
            inner.snapshot = config.clone();
        }
        // Rotate under the same writer lock as `log()`. Failing rotation must
        // not roll back a snapshot that already passed validation: a later
        // write retries rotation, and live settings must not depend on
        // filesystem cleanup.
        if let Some(path) = path.as_ref()
            && let Err(err) = maybe_rotate_log(
                path.as_path(),
                max_log_bytes_from_config(config),
                max_log_age_from_config(config),
            )
        {
            warn!(
                "failed to rotate debug log {} while applying config: {err}",
                path.display()
            );
        }
        Ok(path)
    }

    pub(crate) fn read_tail(
        &self,
        limit: usize,
        query: Option<&str>,
        event: Option<&str>,
    ) -> std::io::Result<JsonlTail> {
        // Hold the writer lock only long enough to pin the current path and
        // open an fd. Rotation can rename the file afterward; this fd still
        // refers to the inode we opened, so parsing does not block writers.
        let (enabled, path, file, file_bytes) = {
            let Ok(_guard) = self.writer_lock.lock() else {
                return Err(std::io::Error::other("failed to lock debug log writer"));
            };
            let inner = self.read_inner();
            let enabled = inner.snapshot.enabled;
            let Some(path) = active_log_path(&inner.snapshot) else {
                return Ok(missing_jsonl_tail(PathBuf::new(), enabled));
            };
            drop(inner);
            match open_debug_log(&path, false) {
                Ok(file) => {
                    let file_bytes = file.metadata()?.len();
                    (enabled, path, file, file_bytes)
                }
                Err(err) if err.kind() == ErrorKind::NotFound => {
                    return Ok(missing_jsonl_tail(path, enabled));
                }
                Err(err) => return Err(err),
            }
        };
        parse_jsonl_tail(file, path, file_bytes, enabled, limit, query, event)
    }

    fn read_inner(&self) -> std::sync::RwLockReadGuard<'_, DebugLogInner> {
        self.inner.read().expect("debug log lock poisoned")
    }

    fn write_inner(&self) -> std::sync::RwLockWriteGuard<'_, DebugLogInner> {
        self.inner.write().expect("debug log lock poisoned")
    }

    pub(crate) fn log_request(&self, mut event: Value, body: &Value) {
        if self.include_bodies()
            && let Some(object) = event.as_object_mut()
        {
            object.insert("body".to_string(), redact_debug_value(body));
        }
        self.log(event);
    }

    pub(crate) fn log_response(&self, mut event: Value, body: Option<&Value>) {
        if self.include_bodies()
            && let Some(body) = body
            && let Some(object) = event.as_object_mut()
        {
            object.insert("body".to_string(), redact_debug_value(body));
        }
        self.log(event);
    }

    pub(crate) fn log_error(&self, mut event: Value, error: &str) {
        if let Some(object) = event.as_object_mut() {
            if self.include_bodies() {
                object.insert("error".to_string(), json!(redact_debug_text(error)));
            } else {
                object.insert(
                    "error_fingerprint".to_string(),
                    json!(text_fingerprint(error)),
                );
                object.insert("error_bytes".to_string(), json!(error.len()));
                object.insert("error_body_redacted".to_string(), json!(true));
            }
        }
        self.log(event);
    }

    pub(crate) fn log_stream_frame(&self, mut event: Value, frame: &str) {
        if self.include_stream_bodies()
            && let Some(object) = event.as_object_mut()
        {
            object.insert("frame".to_string(), json!(redact_debug_text(frame)));
        } else if let Some(object) = event.as_object_mut() {
            object.insert(
                "frame_fingerprint".to_string(),
                json!(text_fingerprint(frame)),
            );
            object.insert("frame_bytes".to_string(), json!(frame.len()));
            object.insert("frame_body_redacted".to_string(), json!(true));
        }
        self.log(event);
    }

    pub(crate) fn log(&self, mut event: Value) {
        let Ok(_guard) = self.writer_lock.lock() else {
            warn!("failed to lock debug log writer");
            return;
        };
        let inner = self.read_inner();
        let Some(path) = active_log_path(&inner.snapshot) else {
            return;
        };
        let max_log_bytes = max_log_bytes_from_config(&inner.snapshot);
        let max_log_age = max_log_age_from_config(&inner.snapshot);
        let include_bodies = inner.snapshot.include_bodies;
        let include_stream_bodies = inner.snapshot.include_stream_bodies;
        drop(inner);
        apply_live_debug_event_policy(&mut event, include_bodies, include_stream_bodies);
        if let Some(object) = event.as_object_mut() {
            object
                .entry("ts".to_string())
                .or_insert_with(|| json!(now_unix_ms()));
            object.insert("schema".to_string(), json!("codex-warp-debug-v1"));
        }
        redact_debug_value_in_place(&mut event);
        if let Err(err) = maybe_rotate_log(path.as_path(), max_log_bytes, max_log_age) {
            warn!("failed to rotate debug log {}: {err}", path.display());
        }
        match open_debug_log(&path, true) {
            Ok(mut file) => {
                if let Err(err) = writeln!(file, "{event}") {
                    warn!("failed to write debug log {}: {err}", path.display());
                }
            }
            Err(err) => warn!("failed to open debug log {}: {err}", path.display()),
        }
    }
}

pub(crate) fn clamp_log_tail_limit(limit: Option<usize>) -> usize {
    limit
        .unwrap_or(DEFAULT_LOG_TAIL_LIMIT)
        .clamp(1, MAX_LOG_TAIL_LIMIT)
}

/// Validate rotation limits and pin `log_path` to a cwd-independent destination.
///
/// Enabled paths must be openable (parent exists, not restricted). Disabled
/// paths still pin through the same resolver when the parent can be resolved,
/// but missing parents and restricted destinations do not fail: a disabled
/// snapshot is not opened and must not fail startup. `..` in a disabled path
/// is left unchanged so enable-time validation can reject it.
pub(crate) fn validate_debug_settings(config: &mut DebugConfig) -> Result<(), String> {
    if let Some(path) = config.log_path.clone() {
        match pin_debug_log_path(&path, config.enabled) {
            Ok(pinned) => config.log_path = Some(pinned),
            Err(err) if config.enabled => return Err(err),
            Err(_) => {}
        }
    } else if config.enabled {
        return Err("debug.log_path is required when debug.enabled is true".to_string());
    }
    if config.max_log_mb == Some(0) {
        return Err("debug.max_log_mb must be greater than 0".to_string());
    }
    if config.max_log_age_days == Some(0) {
        return Err("debug.max_log_age_days must be greater than 0".to_string());
    }
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn validate_debug_log_path(path: &Path) -> Result<PathBuf, String> {
    pin_debug_log_path(path, true)
}

fn pin_debug_log_path(path: &Path, require_usable: bool) -> Result<PathBuf, String> {
    if path.as_os_str().is_empty() {
        return Err("debug log_path is required".to_string());
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("debug log_path must not contain '..'".to_string());
    }
    if require_usable && path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
        return Err("debug log_path must end with .jsonl".to_string());
    }
    if require_usable && is_restricted_log_path(path) {
        return Err("debug log_path is not in an allowed location".to_string());
    }
    let absolute = absolute_debug_log_path(path)?;
    if require_usable && is_restricted_log_path(&absolute) {
        return Err("debug log_path is not in an allowed location".to_string());
    }
    let Some(parent) = absolute
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Err("debug log_path must include a file name".to_string());
    };
    match fs::symlink_metadata(parent) {
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return if require_usable {
                Err("debug log_path parent directory must exist".to_string())
            } else {
                Ok(absolute)
            };
        }
        Err(err) => {
            return if require_usable {
                Err(format!(
                    "debug log_path parent {} is not usable: {err}",
                    parent.display()
                ))
            } else {
                Ok(absolute)
            };
        }
        Ok(metadata) if !metadata.is_dir() && !metadata.file_type().is_symlink() => {
            return if require_usable {
                Err("debug log_path parent must be a directory".to_string())
            } else {
                Ok(absolute)
            };
        }
        Ok(_) => {}
    }
    let canonical_parent = match fs::canonicalize(parent) {
        Ok(parent) => parent,
        Err(err) if require_usable => {
            return Err(format!(
                "debug log_path parent {} is not usable: {err}",
                parent.display()
            ));
        }
        Err(_) => return Ok(absolute),
    };
    if !canonical_parent.is_dir() {
        return if require_usable {
            Err("debug log_path parent must be a directory".to_string())
        } else {
            Ok(absolute)
        };
    }
    let Some(file_name) = absolute.file_name() else {
        return Err("debug log_path must include a file name".to_string());
    };
    let resolved = canonical_parent.join(file_name);
    if require_usable && is_restricted_log_path(&resolved) {
        return Err("debug log_path is not in an allowed location".to_string());
    }
    if !require_usable {
        return Ok(resolved);
    }
    match fs::symlink_metadata(&resolved) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err("debug log_path must not be a symlink".to_string())
        }
        Ok(metadata) if !metadata.is_file() => {
            Err("debug log_path must be a regular file".to_string())
        }
        Ok(_) => Ok(resolved),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(resolved),
        Err(err) => Err(format!(
            "debug log_path {} is not usable: {err}",
            resolved.display()
        )),
    }
}

fn absolute_debug_log_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(path))
        .map_err(|err| {
            format!("resolve debug log_path against the process working directory: {err}")
        })
}

fn is_restricted_log_path(path: &Path) -> bool {
    let mut components = path.components();
    let Some(first) = components.next() else {
        return false;
    };
    if !matches!(first, Component::RootDir | Component::Prefix(_)) {
        return false;
    }
    let name = loop {
        match components.next() {
            Some(Component::Normal(name)) if !name.is_empty() => break name,
            Some(Component::Normal(_)) | Some(Component::RootDir) => continue,
            _ => return false,
        }
    };
    matches!(
        name.to_string_lossy().to_ascii_lowercase().as_str(),
        "etc" | "proc" | "sys" | "dev" | "root"
    )
}

fn debug_log_symlink_error(path: &Path) -> std::io::Error {
    std::io::Error::new(
        ErrorKind::InvalidInput,
        format!("debug log path {} must not be a symlink", path.display()),
    )
}

fn debug_log_not_file_error(path: &Path) -> std::io::Error {
    std::io::Error::new(
        ErrorKind::InvalidInput,
        format!("debug log path {} is not a file", path.display()),
    )
}

fn reject_unusable_debug_log(path: &Path, metadata: &fs::Metadata) -> std::io::Result<()> {
    if metadata.file_type().is_symlink() {
        return Err(debug_log_symlink_error(path));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(debug_log_symlink_error(path));
        }
    }
    if !metadata.is_file() {
        return Err(debug_log_not_file_error(path));
    }
    Ok(())
}

fn open_debug_log(path: &Path, create: bool) -> std::io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => reject_unusable_debug_log(path, &metadata)?,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            if !create {
                return Err(err);
            }
        }
        Err(err) => return Err(err),
    }
    let mut options = OpenOptions::new();
    options.read(true);
    if create {
        options.create(true).append(true);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the named path itself so a symlink cannot redirect writes/tails.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    reject_unusable_debug_log(path, &file.metadata()?)?;
    Ok(file)
}

/// Apply the live snapshot's body-inclusion policy at the write chokepoint.
/// Helpers may attach `body`/`frame`/`error` using a stale unlocked read;
/// `log()` holds `writer_lock` and must decide what actually hits disk.
fn apply_live_debug_event_policy(
    event: &mut Value,
    include_bodies: bool,
    include_stream_bodies: bool,
) {
    let Some(object) = event.as_object_mut() else {
        return;
    };
    if !include_bodies {
        object.remove("body");
        if let Some(Value::String(error)) = object.remove("error") {
            object.insert(
                "error_fingerprint".to_string(),
                json!(text_fingerprint(&error)),
            );
            object.insert("error_bytes".to_string(), json!(error.len()));
            object.insert("error_body_redacted".to_string(), json!(true));
        }
    }
    if !include_stream_bodies && let Some(Value::String(frame)) = object.remove("frame") {
        object.insert(
            "frame_fingerprint".to_string(),
            json!(text_fingerprint(&frame)),
        );
        object.insert("frame_bytes".to_string(), json!(frame.len()));
        object.insert("frame_body_redacted".to_string(), json!(true));
    }
}

fn missing_jsonl_tail(path: PathBuf, enabled: bool) -> JsonlTail {
    JsonlTail {
        path,
        enabled,
        file_bytes: 0,
        truncated: false,
        missing: true,
        events: Vec::new(),
    }
}

#[cfg(test)]
fn read_jsonl_tail(
    path: &Path,
    limit: usize,
    query: Option<&str>,
    event: Option<&str>,
) -> std::io::Result<JsonlTail> {
    let file = match open_debug_log(path, false) {
        Ok(file) => file,
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Ok(missing_jsonl_tail(path.to_path_buf(), true));
        }
        Err(err) => return Err(err),
    };
    let file_bytes = file.metadata()?.len();
    parse_jsonl_tail(
        file,
        path.to_path_buf(),
        file_bytes,
        true,
        limit,
        query,
        event,
    )
}

fn parse_jsonl_tail(
    mut file: File,
    path: PathBuf,
    file_bytes: u64,
    enabled: bool,
    limit: usize,
    query: Option<&str>,
    event: Option<&str>,
) -> std::io::Result<JsonlTail> {
    let start = file_bytes.saturating_sub(LOG_TAIL_READ_BYTES);
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines();
    if start > 0 {
        let _ = lines.next();
    }
    let query = query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let event = event
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase());
    let mut events = Vec::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(event) = event.as_deref() {
            let matches_event = value
                .get("event")
                .and_then(Value::as_str)
                .is_some_and(|name| name.eq_ignore_ascii_case(event));
            if !matches_event {
                continue;
            }
        }
        if let Some(query) = query.as_deref()
            && !line.to_ascii_lowercase().contains(query)
        {
            continue;
        }
        events.push(value);
    }
    if events.len() > limit {
        events = events.split_off(events.len() - limit);
    }
    Ok(JsonlTail {
        path: path.to_path_buf(),
        enabled,
        file_bytes,
        truncated: start > 0,
        missing: false,
        events,
    })
}

pub(crate) fn redact_debug_value(value: &Value) -> Value {
    let mut value = value.clone();
    redact_debug_value_in_place(&mut value);
    value
}

fn redact_debug_value_in_place(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_key(key) {
                    *value = Value::String(REDACTED.to_string());
                } else {
                    redact_debug_value_in_place(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_debug_value_in_place(item);
            }
        }
        Value::String(text) => {
            *text = redact_debug_text(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key == "authorization"
        || key == "api_key"
        || key == "api-key"
        || key == "x-api-key"
        || key == "access_token"
        || key == "refresh_token"
        || key == "password"
        || key == "private_key"
        || key == "signing_key"
        || key.contains("secret")
        || key.ends_with("_token")
        || key.ends_with("_api_key")
}

pub(crate) fn redact_debug_text(text: &str) -> String {
    let mut redacted = redact_assignments(text);
    redacted = redact_bearer_tokens(&redacted);
    redact_prefixed_tokens(&redacted)
}

fn redact_assignments(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < text.len() {
        let remaining = &text[index..];
        if let Some((prefix_len, quote)) = secret_assignment_prefix(remaining) {
            output.push_str(&remaining[..prefix_len]);
            output.push_str(REDACTED);
            index += prefix_len;
            if let Some(quote) = quote {
                let close = text[index..]
                    .find(quote)
                    .map(|offset| index + offset + quote.len_utf8())
                    .unwrap_or(text.len());
                index = close;
            } else {
                while index < text.len() && !bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
            }
        } else {
            let ch = remaining.chars().next().unwrap_or_default();
            output.push(ch);
            index += ch.len_utf8();
        }
    }
    output
}

fn secret_assignment_prefix(text: &str) -> Option<(usize, Option<char>)> {
    let trimmed = text.trim_start();
    let skipped = text.len() - trimmed.len();
    let split = trimmed.find(['=', ':'])?;
    let key = trimmed[..split].trim_matches(['"', '\'', ' ', '\t']);
    if !is_secret_key(key) && !key.to_ascii_uppercase().ends_with("API_KEY") {
        return None;
    }
    let after_separator = &trimmed[split + 1..];
    let spaces = after_separator.len() - after_separator.trim_start().len();
    let value = after_separator.trim_start();
    let quote = value.chars().next().filter(|ch| *ch == '"' || *ch == '\'');
    Some((
        skipped + split + 1 + spaces + quote.map(char::len_utf8).unwrap_or(0),
        quote,
    ))
}

fn redact_bearer_tokens(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        let remaining = &text[index..];
        if starts_with_ascii_ignore_case(remaining, "bearer") {
            output.push_str(&remaining[..6]);
            index += "bearer".len();
            while index < text.len() {
                let ch = text[index..].chars().next().unwrap_or_default();
                if !ch.is_whitespace() {
                    break;
                }
                output.push(ch);
                index += ch.len_utf8();
            }
            if index < text.len() {
                output.push_str(REDACTED);
                while index < text.len() {
                    let ch = text[index..].chars().next().unwrap_or_default();
                    if is_token_boundary(ch) {
                        break;
                    }
                    index += ch.len_utf8();
                }
            }
        } else {
            let ch = remaining.chars().next().unwrap_or_default();
            output.push(ch);
            index += ch.len_utf8();
        }
    }
    output
}

fn redact_prefixed_tokens(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut index = 0;
    while index < text.len() {
        if provider_token_prefix(&text[index..]).is_some() {
            let start = index;
            while index < text.len() {
                let ch = text[index..].chars().next().unwrap_or_default();
                if !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-') {
                    break;
                }
                index += ch.len_utf8();
            }
            if index - start >= 20 {
                output.push_str(REDACTED);
            } else {
                output.push_str(&text[start..index]);
            }
            continue;
        }
        let ch = text[index..].chars().next().unwrap_or_default();
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

fn provider_token_prefix(text: &str) -> Option<&'static str> {
    ["sk-", "sk_", "tp-"]
        .into_iter()
        .find(|prefix| text.starts_with(prefix))
}

fn starts_with_ascii_ignore_case(text: &str, prefix: &str) -> bool {
    text.as_bytes()
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix.as_bytes()))
}

fn is_token_boundary(ch: char) -> bool {
    ch.is_whitespace() || !(ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.')
}

pub(crate) fn request_debug_summary(body: &Value) -> Value {
    json!({
        "model": body.get("model").cloned().unwrap_or(Value::Null),
        "stream": body.get("stream").cloned().unwrap_or(Value::Null),
        "stream_options": body.get("stream_options").cloned().unwrap_or(Value::Null),
        "prompt_cache_key": body.get("prompt_cache_key").cloned().unwrap_or(Value::Null),
        "has_client_metadata": body.get("client_metadata").is_some(),
        "has_metadata": body.get("metadata").is_some(),
        "messages": messages_debug_summary(body),
        "input": input_debug_summary(body),
        "tools": tools_debug_summary(body),
        "response_format_type": body
            .get("response_format")
            .and_then(|format| format.get("type"))
            .cloned()
            .unwrap_or(Value::Null),
        "body_fingerprint": stable_fingerprint(body)
    })
}

fn messages_debug_summary(body: &Value) -> Value {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return Value::Null;
    };
    json!(messages
        .iter()
        .map(|message| json!({
            "role": message.get("role").and_then(Value::as_str).unwrap_or(""),
            "content_fingerprint": stable_fingerprint(message.get("content").unwrap_or(&Value::Null)),
            "content_chars": json_char_len(message.get("content").unwrap_or(&Value::Null)),
            "has_tool_calls": message.get("tool_calls").is_some()
        }))
        .collect::<Vec<_>>())
}

fn input_debug_summary(body: &Value) -> Value {
    let Some(input) = body.get("input") else {
        return Value::Null;
    };
    json!({
        "fingerprint": stable_fingerprint(input),
        "chars": json_char_len(input)
    })
}

fn tools_debug_summary(body: &Value) -> Value {
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return Value::Null;
    };
    json!({
        "count": tools.len(),
        "fingerprint": stable_fingerprint(&Value::Array(tools.clone()))
    })
}

fn json_char_len(value: &Value) -> usize {
    match value {
        Value::String(value) => value.chars().count(),
        _ => value.to_string().chars().count(),
    }
}

/// NOTE: `DefaultHasher` fingerprints are NOT stable across process restarts or platforms.
/// These fingerprints are used for debug-log-only purposes and should not be used
/// for cross-session correlation or deduplication.
fn stable_fingerprint(value: &Value) -> String {
    let mut hasher = DefaultHasher::new();
    value.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) fn text_fingerprint(value: &str) -> String {
    // `DefaultHasher` fingerprints are debug-only and are not stable across
    // process restarts or platforms.
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
#[path = "debug_log_tests.rs"]
mod tests;
