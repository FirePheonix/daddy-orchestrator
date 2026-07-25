use crate::config::{AgentOptions, RunOptions};
use crate::models::{ContentBlock, Trajectory, Turn};
use crate::provider::{resolve_cwd, ProviderCatalog, ProviderRequest};
use anyhow::{anyhow, Result};
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
            .unwrap_or_else(|| vec!["claude".to_string(), "codex".to_string(), "opencode".to_string()]);
        let provider = self
            .catalog
            .first_installed(&order)
            .ok_or_else(|| anyhow!("no providers registered"))?;
        let cwd = resolve_cwd(self.options.cwd.as_deref())?;
        let model = self
            .options
            .model
            .clone()
            .or_else(|| self.options.model_tier.as_ref().and_then(|tier| provider.resolve_model(tier)));
        let mut trajectory = Trajectory::new(provider.name(), model.clone().unwrap_or_default(), Uuid::new_v4().to_string());
        trajectory.system_prompt = self.options.system_prompt.clone().unwrap_or_default();
        trajectory.reasoning = self.options.reasoning.clone().unwrap_or_default();
        trajectory.mcp_servers = self.options.mcp_servers.clone();
        trajectory.metadata = self.options.metadata.clone();
        Ok(Session {
            provider,
            cwd,
            model,
            model_tier: self.options.model_tier.clone(),
            reasoning: self.options.reasoning.clone(),
            system_prompt: self.options.system_prompt.clone(),
            mcp_servers: self.options.mcp_servers.clone(),
            data_dir: self.options.data_dir.clone(),
            trajectory,
            traj_path: run.traj_path,
            last_raw_output: None,
        })
    }

    pub fn completion(&self, message: &str, run: RunOptions) -> Result<Trajectory> {
        let mut session = self.start_session(run)?;
        session.send(message)?;
        session.end()
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
        let prompt = render_replay_prompt(self.system_prompt.as_deref(), &self.trajectory.turns, message);
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
        std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(&self.trajectory)?))?;
        Ok(())
    }

    pub fn end(mut self) -> Result<Trajectory> {
        self.trajectory.finalize();
        if let Some(path) = self.traj_path.take() {
            self.save_trajectory(path)?;
        }
        if let Some(data_dir) = &self.data_dir {
            let session_dir = data_dir.join("sessions").join(&self.trajectory.session_id);
            std::fs::create_dir_all(&session_dir)?;
            let path = session_dir.join("trajectory.json");
            self.save_trajectory(path)?;
        }
        Ok(self.trajectory)
    }

    pub fn resume_handle(&self) -> String {
        self.trajectory.session_id.clone()
    }
}

fn render_replay_prompt(system_prompt: Option<&str>, history: &[Turn], next_message: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UsageStats;
    use crate::provider::{Provider, ProviderCatalog, ProviderRequest, ProviderResponse};
    use std::collections::BTreeMap;

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
                text: format!("echo: {}", request.prompt.lines().last().unwrap_or_default()),
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
}
