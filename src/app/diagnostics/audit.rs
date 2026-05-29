//! Opt-in route-decision audit log.
//!
//! When `LSB_ROUTE_AUDIT=1` is set in the environment, every PipeWire metadata
//! write Soundboard makes (per-stream `target.object`/`target.node`) and every
//! default-source claim/restore command is appended as a JSONL record to
//! `$XDG_RUNTIME_DIR/linux-soundboard-route-audit.log` (falling back to
//! `/tmp/linux-soundboard-route-audit.log`).
//!
//! When the env var is not set the audit channel is never initialised and the
//! `record_*` hot paths short-circuit on a single atomic load (see
//! `is_enabled()`), so leaving this code in tree carries no runtime cost.
//!
//! The audit log is a debug aid for reproducing the "Auto-route mode breaks
//! Vesktop screen-share-with-sound" regression — see
//! `docs/TROUBLESHOOTING.md` "Capturing Auto-route audit data".

use parking_lot::Mutex;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::OnceLock;

use serde_json::{json, Value};

const ENV_VAR: &str = "LSB_ROUTE_AUDIT";
const FILE_NAME: &str = "linux-soundboard-route-audit.log";

static AUDIT: OnceLock<AuditWriter> = OnceLock::new();

pub struct AuditWriter {
    path: PathBuf,
    file: Mutex<BufWriter<File>>,
}

impl AuditWriter {
    fn open(path: PathBuf) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(BufWriter::new(file)),
        })
    }

    fn write_record(&self, kind: &str, fields: Value) {
        let line = build_record_line(kind, fields);
        let mut guard = self.file.lock();
        if guard.write_all(line.as_bytes()).is_ok() {
            let _ = guard.flush();
        }
    }
}

/// Initialise the audit channel from the environment. Idempotent — calling
/// twice is a no-op. Safe to call on any thread.
pub fn init_from_env() {
    if !env_enabled() {
        return;
    }
    let path = audit_log_path();
    match AuditWriter::open(path.clone()) {
        Ok(writer) => {
            // Ignore the error from `set` — that just means another thread
            // got there first, which is fine.
            let _ = AUDIT.set(writer);
            if let Some(writer) = AUDIT.get() {
                log::info!("Route-audit log enabled at {}", writer.path.display());
            }
        }
        Err(err) => {
            log::warn!(
                "Could not open route-audit log at {}: {err}",
                path.display()
            );
        }
    }
}

pub fn is_enabled() -> bool {
    AUDIT.get().is_some()
}

fn env_enabled() -> bool {
    std::env::var(ENV_VAR)
        .map(|v| {
            let v = v.trim().to_ascii_lowercase();
            !v.is_empty() && v != "0" && v != "false" && v != "off" && v != "no"
        })
        .unwrap_or(false)
}

fn audit_log_path() -> PathBuf {
    if let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join(FILE_NAME);
    }
    std::env::temp_dir().join(FILE_NAME)
}

/// Record a PipeWire `default`-metadata write Soundboard performed (or
/// cleared). `before`/`after` carry the prior and new `target.object` value.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn record_metadata_write(
    stream_id: u32,
    app: Option<&str>,
    binary: Option<&str>,
    media_name: Option<&str>,
    media_role: Option<&str>,
    before: Option<&str>,
    after: Option<&str>,
    reason: &str,
) {
    let Some(writer) = AUDIT.get() else {
        return;
    };
    let kind = if after.is_some() {
        "metadata.set"
    } else {
        "metadata.clear"
    };
    writer.write_record(
        kind,
        json!({
            "stream_id": stream_id,
            "app": app,
            "binary": binary,
            "media_name": media_name,
            "media_role": media_role,
            "key": "target.object",
            "before": before,
            "after": after,
            "reason": reason,
        }),
    );
}

/// Record a default-source claim or restore (`wpctl set-default` / `pactl
/// set-default-source`). `kind` is one of `"default_source.claim"` /
/// `"default_source.restore"` / `"default_source.pulse_claim"` /
/// `"default_source.pulse_restore"`.
// Called from #[cfg(not(test))] blocks in source_routing.rs; clippy's test
// target treats it as dead code but it is live in regular builds.
#[allow(dead_code)]
pub fn record_default_source_command(
    kind: &str,
    source_id: Option<u32>,
    source_name: Option<&str>,
    outcome: Result<(), &str>,
) {
    let Some(writer) = AUDIT.get() else {
        return;
    };
    let outcome_value = match outcome {
        Ok(()) => json!({"ok": true}),
        Err(err) => json!({"ok": false, "error": err}),
    };
    writer.write_record(
        kind,
        json!({
            "source_id": source_id,
            "source_name": source_name,
            "outcome": outcome_value,
        }),
    );
}

fn build_record_line(kind: &str, fields: Value) -> String {
    let mut record = serde_json::Map::new();
    record.insert("ts".to_string(), Value::String(timestamp_iso8601()));
    record.insert("kind".to_string(), Value::String(kind.to_string()));
    if let Value::Object(map) = fields {
        for (k, v) in map {
            record.insert(k, v);
        }
    }
    let mut line = serde_json::to_string(&Value::Object(record))
        .unwrap_or_else(|_| String::from("{\"ts\":null,\"kind\":\"<serialize-error>\"}"));
    line.push('\n');
    line
}

fn timestamp_iso8601() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn build_record_line_includes_ts_kind_and_payload() {
        let line = build_record_line(
            "metadata.set",
            json!({
                "stream_id": 4618,
                "app": "Chromium input",
                "binary": "vesktop",
                "media_name": "RecordStream",
                "media_role": null,
                "key": "target.object",
                "before": null,
                "after": "linuxsoundboard.virtual_mic",
                "reason": "explicit upstream target",
            }),
        );
        assert!(line.ends_with('\n'));
        let parsed: Value = serde_json::from_str(line.trim()).expect("valid JSON");
        let map = parsed.as_object().expect("object record");
        assert_eq!(
            map.get("kind").and_then(|v| v.as_str()),
            Some("metadata.set")
        );
        assert_eq!(map.get("stream_id").and_then(|v| v.as_u64()), Some(4618));
        assert_eq!(
            map.get("app").and_then(|v| v.as_str()),
            Some("Chromium input")
        );
        assert_eq!(map.get("binary").and_then(|v| v.as_str()), Some("vesktop"));
        assert_eq!(
            map.get("after").and_then(|v| v.as_str()),
            Some("linuxsoundboard.virtual_mic")
        );
        assert_eq!(map.get("before"), Some(&Value::Null));
        let ts = map.get("ts").and_then(|v| v.as_str()).expect("ts string");
        // Loose ISO8601 sanity: contains 'T', ends with 'Z', has a 4-digit year.
        assert!(ts.contains('T') && ts.ends_with('Z'), "ts={ts}");
        assert!(ts.split_once('-').is_some_and(|(year, _)| year.len() == 4));
    }

    #[test]
    fn build_record_line_for_metadata_clear_records_after_null() {
        let line = build_record_line(
            "metadata.clear",
            json!({
                "stream_id": 4688,
                "app": "Chromium input",
                "binary": "vesktop",
                "before": "linuxsoundboard.virtual_mic",
                "after": null,
                "reason": "filter changed",
                "key": "target.object",
            }),
        );
        let parsed: Value = serde_json::from_str(line.trim()).expect("valid JSON");
        assert_eq!(
            parsed.get("after"),
            Some(&Value::Null),
            "metadata.clear must record null after"
        );
        assert_eq!(
            parsed.get("kind").and_then(|v| v.as_str()),
            Some("metadata.clear")
        );
    }

    #[test]
    fn env_enabled_recognises_truthy_values() {
        let _guard = ENV_TEST_LOCK.lock().expect("env test lock");
        for val in ["1", "true", "TRUE", "yes", "on"] {
            std::env::set_var(ENV_VAR, val);
            assert!(env_enabled(), "expected env_enabled() for {val:?}");
        }
        for val in ["", "0", "false", "off", "no"] {
            std::env::set_var(ENV_VAR, val);
            assert!(!env_enabled(), "expected !env_enabled() for {val:?}");
        }
        std::env::remove_var(ENV_VAR);
        assert!(!env_enabled(), "expected !env_enabled() when unset");
    }

    #[test]
    fn audit_writer_appends_records_to_file() {
        let dir = tempdir();
        let path = dir.join("audit.log");
        let writer = AuditWriter::open(path.clone()).expect("open audit file");
        writer.write_record("metadata.set", json!({"a": 1}));
        writer.write_record("metadata.clear", json!({"b": 2}));
        // Drop to ensure flush via BufWriter destructor.
        drop(writer);
        let content = std::fs::read_to_string(&path).expect("read audit file");
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "got: {content:?}");
        let first: Value = serde_json::from_str(lines[0]).expect("line 0 JSON");
        assert_eq!(
            first.get("kind").and_then(|v| v.as_str()),
            Some("metadata.set")
        );
        let second: Value = serde_json::from_str(lines[1]).expect("line 1 JSON");
        assert_eq!(second.get("b").and_then(|v| v.as_u64()), Some(2));
    }

    #[test]
    fn is_enabled_is_false_when_env_unset_and_init_skipped() {
        let _guard = ENV_TEST_LOCK.lock().expect("env test lock");
        // We can't easily unset the global OnceLock, but we can verify the
        // recorders themselves no-op when AUDIT is unset by writing through
        // the public API and asserting no file at the audit path was created.
        std::env::remove_var(ENV_VAR);
        let dir = tempdir();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        // Don't call init_from_env(); confirm record_metadata_write is a noop.
        record_metadata_write(
            42,
            Some("App"),
            Some("bin"),
            Some("media"),
            None,
            None,
            Some("linuxsoundboard.virtual_mic"),
            "test",
        );
        let audit_path = dir.join(FILE_NAME);
        assert!(
            !audit_path.exists()
                || std::fs::read_to_string(&audit_path)
                    .unwrap_or_default()
                    .is_empty(),
            "audit file should not have been touched when env is unset"
        );
    }

    fn tempdir() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lsb-audit-test-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        std::fs::create_dir_all(&path).expect("create test dir");
        path
    }
}
