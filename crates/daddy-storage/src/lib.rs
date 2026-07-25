use anyhow::Result;
use daddy_core::{ContentBlock, Trajectory, Turn};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct SessionStore {
    root: PathBuf,
    turns_dir: PathBuf,
    turn_counter: usize,
}

impl SessionStore {
    pub fn new(data_dir: impl AsRef<Path>, session_id: &str) -> Result<Self> {
        let root = data_dir.as_ref().join("sessions").join(session_id);
        let turns_dir = root.join("turns");
        fs::create_dir_all(&turns_dir)?;
        Ok(Self {
            root,
            turns_dir,
            turn_counter: 0,
        })
    }

    pub fn session_dir(&self) -> &Path {
        &self.root
    }

    pub fn write_metadata(&self, trajectory: &Trajectory) -> Result<()> {
        self.append_jsonl(&serde_json::json!({
            "type": "metadata",
            "session_id": trajectory.session_id,
            "created_at": trajectory.created_at,
            "agent": trajectory.agent,
            "model": trajectory.model,
            "system_prompt": trajectory.system_prompt,
            "reasoning": trajectory.reasoning,
            "mcp_servers": trajectory.mcp_servers,
            "metadata": trajectory.metadata,
        }))
    }

    pub fn append_turn(&mut self, turn: &Turn, trajectory: &Trajectory, raw_output: Option<&str>) -> Result<()> {
        let prefix = format!("{:03}", self.turn_counter);
        fs::write(self.turns_dir.join(format!("{prefix}_input.txt")), &turn.input)?;
        if let Some(raw_output) = raw_output {
            fs::write(
                self.turns_dir.join(format!("{prefix}_raw_output.txt")),
                raw_output,
            )?;
        }

        self.append_jsonl(&serde_json::json!({
            "type": "user",
            "turn_index": self.turn_counter,
            "message": turn.input,
        }))?;

        for block in &turn.output {
            match block {
                ContentBlock::Text { text } => self.append_jsonl(&serde_json::json!({
                    "type": "text",
                    "turn_index": self.turn_counter,
                    "text": text,
                }))?,
                ContentBlock::Thinking { text } => self.append_jsonl(&serde_json::json!({
                    "type": "thinking",
                    "turn_index": self.turn_counter,
                    "text": text,
                }))?,
                ContentBlock::ToolUse(tool) => {
                    self.append_jsonl(&serde_json::json!({
                        "type": "tool_call",
                        "turn_index": self.turn_counter,
                        "id": tool.id,
                        "name": tool.name,
                        "arguments": tool.arguments,
                    }))?;
                    self.append_jsonl(&serde_json::json!({
                        "type": "tool_result",
                        "turn_index": self.turn_counter,
                        "id": tool.id,
                        "name": tool.name,
                        "output": tool.output,
                        "is_error": tool.is_error,
                    }))?;
                }
            }
        }

        self.append_jsonl(&serde_json::json!({
            "type": "turn_end",
            "turn_index": self.turn_counter,
            "usage": turn.usage,
            "duration_ms": turn.duration_ms,
        }))?;

        self.save_trajectory(trajectory)?;
        self.turn_counter += 1;
        Ok(())
    }

    pub fn save_trajectory(&self, trajectory: &Trajectory) -> Result<()> {
        atomic_write_json(self.root.join("trajectory.json"), trajectory)
    }

    fn append_jsonl(&self, value: &impl Serialize) -> Result<()> {
        use std::io::Write;
        let path = self.root.join("traj.jsonl");
        let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{}", serde_json::to_string(value)?)?;
        Ok(())
    }
}

pub fn load_trajectory(path: impl AsRef<Path>) -> Result<Trajectory> {
    let content = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&content)?)
}

pub fn inspect_trajectory(path: impl AsRef<Path>) -> Result<String> {
    let trajectory = load_trajectory(path)?;
    let mut out = Vec::new();
    out.push(format!("provider: {}", trajectory.agent));
    out.push(format!(
        "model: {}",
        if trajectory.model.is_empty() {
            "<default>"
        } else {
            trajectory.model.as_str()
        }
    ));
    out.push(format!("session_id: {}", trajectory.session_id));
    out.push(format!("turns: {}", trajectory.turns.len()));
    out.push(format!("tool_calls: {}", trajectory.total_tool_calls()));
    out.push(format!("tokens: {}", trajectory.usage.total_tokens()));
    out.push(format!("cost_usd: {:.4}", trajectory.usage.cost_usd));
    out.push(String::new());
    for (idx, turn) in trajectory.turns.iter().enumerate() {
        out.push(format!("[{}] user: {}", idx + 1, preview(&turn.input)));
        out.push(format!("[{}] assistant: {}", idx + 1, preview(&turn.result())));
    }
    Ok(out.join("\n"))
}

fn preview(text: &str) -> String {
    let compact = text.replace('\n', " ");
    if compact.chars().count() > 100 {
        format!("{}...", compact.chars().take(100).collect::<String>())
    } else {
        compact
    }
}

fn atomic_write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(tmp.as_file_mut(), value)?;
    use std::io::Write;
    writeln!(tmp.as_file_mut())?;
    tmp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspector_renders_summary() {
        let temp = tempfile::tempdir().unwrap();
        let mut trajectory = Trajectory::new("mock", "model", "session");
        trajectory.append_turn(Turn {
            input: "hello".to_string(),
            output: vec![ContentBlock::Text {
                text: "world".to_string(),
            }],
            usage: Default::default(),
            duration_ms: 1,
        });
        let path = temp.path().join("trajectory.json");
        atomic_write_json(&path, &trajectory).unwrap();
        let text = inspect_trajectory(path).unwrap();
        assert!(text.contains("provider: mock"));
        assert!(text.contains("[1] assistant: world"));
    }
}
