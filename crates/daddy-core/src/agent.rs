use crate::config::{AgentOptions, RunOptions};
use crate::models::{ContentBlock, Trajectory, Turn};
use crate::provider::{ProviderCatalog, ProviderRequest, resolve_cwd};
use anyhow::{Result, anyhow};
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

pub struct AgentBuilder {
    catalog: Arc<dyn ProviderCatalog>,
    options: AgentOptions,
}

impl AgentBuilder {
    pub fn new(catalog: Arc<dyn ProviderCatalog>) -> Self {
        Self {
            catalog,
            options: AgentOptions::default(),
        }
    }

    pub fn with_options(mut self, options: AgentOptions) -> Self {
        self.options = options;
        self
    }

    pub fn build(self) -> Agent {
        Agent {
            catalog: self.catalog,
            options: self.options,
        }
    }
}

#[derive(Clone)]
pub struct Agent {
    catalog: Arc<dyn ProviderCatalog>,
    options: AgentOptions,
}

impl Agent {
    pub fn builder(catalog: Arc<dyn ProviderCatalog>) -> AgentBuilder {
        AgentBuilder::new(catalog)
    }

    pub fn start_session(&self, run: RunOptions) -> Result<Session> {
        let order = self
            .options
            .provider
            .clone()
            .or_else(provider_order_from_env)
            .unwrap_or_else(|| {
                vec![
                    "claude".to_string(),
                    "codex".to_string(),
                    "opencode".to_string(),
                ]
            });
        let provider = self
            .catalog
            .first_installed(&order)
            .ok_or_else(|| anyhow!("no providers registered"))?;
        let cwd_override = self
            .options
            .cwd
            .clone()
            .or_else(|| std::env::var("DADDY_CWD").ok().map(PathBuf::from));
        let effective_model_tier = self.options.model_tier.clone().or_else(model_tier_from_env);
        let cwd = resolve_cwd(cwd_override.as_deref())?;
        let model = self
            .options
            .model
            .clone()
            .or_else(|| std::env::var("DADDY_MODEL").ok())
            .or_else(|| {
                effective_model_tier
                    .as_ref()
                    .and_then(|tier| provider.resolve_model(tier))
            });
        let reasoning = self
            .options
            .reasoning
            .clone()
            .or_else(|| std::env::var("DADDY_REASONING").ok());
        let system_prompt = self
            .options
            .system_prompt
            .clone()
            .or_else(|| std::env::var("DADDY_SYSTEM_PROMPT").ok());
        let data_dir = self
            .options
            .data_dir
            .clone()
            .or_else(|| std::env::var("DADDY_DATA_DIR").ok().map(PathBuf::from));
        let mut trajectory = Trajectory::new(
            provider.name(),
            model.clone().unwrap_or_default(),
            Uuid::new_v4().to_string(),
        );
        trajectory.system_prompt = system_prompt.clone().unwrap_or_default();
        trajectory.reasoning = reasoning.clone().unwrap_or_default();
        trajectory.mcp_servers = self.options.mcp_servers.clone();
        trajectory.metadata = self.options.metadata.clone();
        let session = Session {
            provider,
            cwd,
            model,
            model_tier: effective_model_tier,
            reasoning,
            system_prompt,
            mcp_servers: self.options.mcp_servers.clone(),
            data_dir,
            trajectory,
            traj_path: run.traj_path,
            last_raw_output: None,
        };
        session.write_metadata()?;
        Ok(session)
    }

    pub fn completion(&self, message: &str, run: RunOptions) -> Result<Trajectory> {
        let mut session = self.start_session(run)?;
        session.send(message)?;
        session.end()
    }
}

fn provider_order_from_env() -> Option<Vec<String>> {
    let raw = std::env::var("DADDY_PROVIDER").ok()?;
    let values: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn model_tier_from_env() -> Option<crate::models::ModelTier> {
    match std::env::var("DADDY_MODEL_TIER").ok()?.as_str() {
        "strongest" => Some(crate::models::ModelTier::Strongest),
        "fast" => Some(crate::models::ModelTier::Fast),
        _ => None,
    }
}

pub struct Session {
    provider: Arc<dyn crate::provider::Provider>,
    cwd: PathBuf,
    model: Option<String>,
    model_tier: Option<crate::models::ModelTier>,
    reasoning: Option<String>,
    system_prompt: Option<String>,
    mcp_servers: Vec<crate::models::MCPServer>,
    data_dir: Option<PathBuf>,
    trajectory: Trajectory,
    traj_path: Option<PathBuf>,
    last_raw_output: Option<String>,
}

impl Session {
    pub fn from_trajectory(
        provider: Arc<dyn crate::provider::Provider>,
        path: impl AsRef<Path>,
        cwd: Option<PathBuf>,
    ) -> Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let trajectory: Trajectory = serde_json::from_str(&content)?;
        Ok(Self {
            provider,
            cwd: cwd.unwrap_or(std::env::current_dir()?),
            model: if trajectory.model.is_empty() {
                None
            } else {
                Some(trajectory.model.clone())
            },
            model_tier: None,
            reasoning: if trajectory.reasoning.is_empty() {
                None
            } else {
                Some(trajectory.reasoning.clone())
            },
            system_prompt: if trajectory.system_prompt.is_empty() {
                None
            } else {
                Some(trajectory.system_prompt.clone())
            },
            mcp_servers: trajectory.mcp_servers.clone(),
            data_dir: None,
            trajectory,
            traj_path: None,
            last_raw_output: None,
        })
    }

    pub fn trajectory(&self) -> &Trajectory {
        &self.trajectory
    }

    pub fn last_raw_output(&self) -> Option<&str> {
        self.last_raw_output.as_deref()
    }

    pub fn send(&mut self, message: &str) -> Result<Turn> {
        let prompt = render_replay_prompt(
            self.system_prompt.as_deref(),
            &self.trajectory.turns,
            message,
        );
        let response = self.provider.execute(&ProviderRequest {
            prompt,
            model: self.model.clone(),
            model_tier: self.model_tier.clone(),
            reasoning: self.reasoning.clone(),
            system_prompt: self.system_prompt.clone(),
            cwd: self.cwd.clone(),
            mcp_servers: self.mcp_servers.clone(),
        })?;
        let turn = Turn {
            input: message.to_string(),
            output: vec![ContentBlock::Text {
                text: response.text.clone(),
            }],
            usage: response.usage.clone(),
            duration_ms: 0,
        };
        self.last_raw_output = Some(response.raw_output);
        self.trajectory.append_turn(turn.clone());
        self.persist_turn(&turn)?;
        if let Some(path) = &self.traj_path {
            self.save_trajectory(path)?;
        }
        Ok(turn)
    }

    pub fn save_trajectory(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            path,
            format!("{}\n", serde_json::to_string_pretty(&self.trajectory)?),
        )?;
        Ok(())
    }

    pub fn end(mut self) -> Result<Trajectory> {
        self.trajectory.finalize();
        if let Some(path) = self.traj_path.take() {
            self.save_trajectory(path)?;
        }
        if let Some(data_dir) = &self.data_dir {
            let path = data_dir
                .join("sessions")
                .join(&self.trajectory.session_id)
                .join("trajectory.json");
            self.save_trajectory(path)?;
        }
        Ok(self.trajectory)
    }

    pub fn resume_handle(&self) -> String {
        self.trajectory.session_id.clone()
    }

    pub fn resume_from_data_dir(
        provider: Arc<dyn crate::provider::Provider>,
        data_dir: impl AsRef<Path>,
        session_id: &str,
    ) -> Result<Self> {
        let path = data_dir
            .as_ref()
            .join("sessions")
            .join(session_id)
            .join("trajectory.json");
        Self::from_trajectory(provider, path, None)
    }

    fn write_metadata(&self) -> Result<()> {
        let Some(root) = self.session_root() else {
            return Ok(());
        };
        fs::create_dir_all(root.join("turns"))?;
        self.append_jsonl(&serde_json::json!({
            "type": "metadata",
            "session_id": self.trajectory.session_id,
            "created_at": self.trajectory.created_at,
            "agent": self.trajectory.agent,
            "model": self.trajectory.model,
            "system_prompt": self.trajectory.system_prompt,
            "reasoning": self.trajectory.reasoning,
            "mcp_servers": self.trajectory.mcp_servers,
            "metadata": self.trajectory.metadata,
        }))
    }

    fn persist_turn(&self, turn: &Turn) -> Result<()> {
        let Some(root) = self.session_root() else {
            return Ok(());
        };
        let turn_index = self.trajectory.turns.len().saturating_sub(1);
        let turns_dir = root.join("turns");
        fs::create_dir_all(&turns_dir)?;
        let prefix = format!("{turn_index:03}");
        fs::write(turns_dir.join(format!("{prefix}_input.txt")), &turn.input)?;
        if let Some(raw_output) = &self.last_raw_output {
            fs::write(
                turns_dir.join(format!("{prefix}_raw_output.txt")),
                raw_output,
            )?;
        }
        self.append_jsonl(&serde_json::json!({
            "type": "user",
            "turn_index": turn_index,
            "message": turn.input,
        }))?;
        for block in &turn.output {
            match block {
                ContentBlock::Text { text } => self.append_jsonl(&serde_json::json!({
                    "type": "text",
                    "turn_index": turn_index,
                    "text": text,
                }))?,
                ContentBlock::Thinking { text } => self.append_jsonl(&serde_json::json!({
                    "type": "thinking",
                    "turn_index": turn_index,
                    "text": text,
                }))?,
                ContentBlock::ToolUse(tool) => {
                    self.append_jsonl(&serde_json::json!({
                        "type": "tool_call",
                        "turn_index": turn_index,
                        "id": tool.id,
                        "name": tool.name,
                        "arguments": tool.arguments,
                    }))?;
                    self.append_jsonl(&serde_json::json!({
                        "type": "tool_result",
                        "turn_index": turn_index,
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
            "turn_index": turn_index,
            "usage": turn.usage,
            "duration_ms": turn.duration_ms,
        }))?;
        atomic_write_json(root.join("trajectory.json"), &self.trajectory)?;
        Ok(())
    }

    fn append_jsonl(&self, value: &impl Serialize) -> Result<()> {
        let Some(root) = self.session_root() else {
            return Ok(());
        };
        fs::create_dir_all(&root)?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root.join("traj.jsonl"))?;
        writeln!(file, "{}", serde_json::to_string(value)?)?;
        Ok(())
    }

    fn session_root(&self) -> Option<PathBuf> {
        self.data_dir
            .as_ref()
            .map(|data_dir| data_dir.join("sessions").join(&self.trajectory.session_id))
    }
}

fn render_replay_prompt(
    system_prompt: Option<&str>,
    history: &[Turn],
    next_message: &str,
) -> String {
    let mut rendered = String::new();
    if let Some(system_prompt) = system_prompt.filter(|text| !text.trim().is_empty()) {
        rendered.push_str("System instructions:\n");
        rendered.push_str(system_prompt.trim());
        rendered.push_str("\n\n");
    }

    if !history.is_empty() {
        rendered.push_str("Conversation so far:\n");
        for turn in history {
            rendered.push_str("User:\n");
            rendered.push_str(turn.input.trim());
            rendered.push_str("\n\nAssistant:\n");
            rendered.push_str(turn.result().trim());
            rendered.push_str("\n\n");
        }
    }

    rendered.push_str("User:\n");
    rendered.push_str(next_message.trim());
    rendered.push_str("\n\nAssistant:");
    rendered
}

fn atomic_write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer_pretty(tmp.as_file_mut(), value)?;
    writeln!(tmp.as_file_mut())?;
    tmp.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UsageStats;
    use crate::provider::{Provider, ProviderCatalog, ProviderRequest, ProviderResponse};
    use std::collections::BTreeMap;
    use std::sync::{Mutex, OnceLock};

    struct MockProvider;

    impl Provider for MockProvider {
        fn name(&self) -> &'static str {
            "mock"
        }

        fn binary_name(&self) -> &'static str {
            "mock"
        }

        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: format!(
                    "echo: {}",
                    request.prompt.lines().last().unwrap_or_default()
                ),
                raw_output: "raw".to_string(),
                usage: UsageStats::default(),
            })
        }

        fn find_binary(&self) -> Option<PathBuf> {
            Some(PathBuf::from("mock"))
        }
    }

    struct MockCatalog;

    impl ProviderCatalog for MockCatalog {
        fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
            if name == "mock" {
                Some(Arc::new(MockProvider))
            } else {
                None
            }
        }

        fn providers(&self) -> Vec<Arc<dyn Provider>> {
            vec![Arc::new(MockProvider)]
        }
    }

    #[test]
    fn session_replays_history() {
        let mut metadata = BTreeMap::new();
        metadata.insert("suite".to_string(), serde_json::json!("core"));
        let agent = Agent::builder(Arc::new(MockCatalog))
            .with_options(AgentOptions {
                provider: Some(vec!["mock".to_string()]),
                metadata,
                ..Default::default()
            })
            .build();
        let mut session = agent.start_session(RunOptions::default()).unwrap();
        session.send("one").unwrap();
        session.send("two").unwrap();
        assert_eq!(session.trajectory.turns.len(), 2);
        assert!(session.trajectory.result().contains("Assistant:"));
    }

    #[test]
    fn session_persists_turn_files_when_data_dir_is_set() {
        let temp = tempfile::tempdir().unwrap();
        let agent = Agent::builder(Arc::new(MockCatalog))
            .with_options(AgentOptions {
                provider: Some(vec!["mock".to_string()]),
                data_dir: Some(temp.path().to_path_buf()),
                ..Default::default()
            })
            .build();
        let mut session = agent.start_session(RunOptions::default()).unwrap();
        session.send("persist me").unwrap();
        let session_dir = temp.path().join("sessions").join(session.resume_handle());
        assert!(session_dir.join("traj.jsonl").exists());
        assert!(session_dir.join("trajectory.json").exists());
        assert!(session_dir.join("turns").join("000_input.txt").exists());
    }

    #[test]
    fn provider_order_can_come_from_env() {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        let guard = LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
        let _guard = guard;
        unsafe {
            std::env::set_var("DADDY_PROVIDER", "mock");
        }
        let agent = Agent::builder(Arc::new(MockCatalog)).build();
        let session = agent.start_session(RunOptions::default()).unwrap();
        assert_eq!(session.trajectory.agent, "mock");
        unsafe {
            std::env::remove_var("DADDY_PROVIDER");
        }
    }
}
