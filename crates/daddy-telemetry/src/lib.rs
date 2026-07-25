use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TelemetryEvent {
    pub timestamp: DateTime<Utc>,
    pub event_type: String,
    #[serde(default)]
    pub job_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub payload: serde_json::Value,
}

pub struct FileTelemetryRecorder {
    path: PathBuf,
}

impl FileTelemetryRecorder {
    // Create a JSONL telemetry recorder that writes events to the given path.
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    // Append one structured event to the telemetry log.
    pub fn record(&self, event: &TelemetryEvent) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        use std::io::Write;
        writeln!(file, "{}", serde_json::to_string(event)?)?;
        Ok(())
    }

    // Return the path used by this recorder.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

// Build one telemetry event with the current timestamp and structured payload.
pub fn telemetry_event(
    event_type: &str,
    job_id: Option<&str>,
    task_id: Option<&str>,
    provider: Option<&str>,
    payload: serde_json::Value,
) -> TelemetryEvent {
    TelemetryEvent {
        timestamp: Utc::now(),
        event_type: event_type.to_string(),
        job_id: job_id.map(ToString::to_string),
        task_id: task_id.map(ToString::to_string),
        provider: provider.map(ToString::to_string),
        payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Persist one JSONL telemetry event so orchestrated runs can be replayed and inspected later.
    fn recorder_writes_jsonl_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let recorder = FileTelemetryRecorder::new(&path);
        recorder
            .record(&telemetry_event(
                "task_completed",
                Some("job-1"),
                Some("task-1"),
                Some("codex"),
                serde_json::json!({"ok": true}),
            ))
            .unwrap();
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("\"task_completed\""));
    }
}
