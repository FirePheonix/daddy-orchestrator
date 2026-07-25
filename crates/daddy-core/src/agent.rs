use crate::config::{AgentOptions, RunOptions};
use crate::models::{ContentBlock, Trajectory, Turn};
use crate::provider::{ProviderCatalog, ProviderRequest, ResumeContext, resolve_cwd};
use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::Value;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ResumeHandleData {
    version: u8,
    provider: String,
    session_id: String,
    #[serde(default)]
    provider_session_id: Option<String>,
    #[serde(default)]
    provider_resume_key: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    system_prompt: Option<String>,
}

pub struct AgentBuilder {
    catalog: Arc<dyn ProviderCatalog>,
    options: AgentOptions,
}

impl AgentBuilder {
    // Create a builder around a provider catalog.
    pub fn new(catalog: Arc<dyn ProviderCatalog>) -> Self {
        Self {
            catalog,
            options: AgentOptions::default(),
        }
    }

    // Override the default agent options before building.
    pub fn with_options(mut self, options: AgentOptions) -> Self {
        self.options = options;
        self
    }

    // Finalize the builder into an Agent.
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
    // Start building a new Agent with the supplied provider catalog.
    pub fn builder(catalog: Arc<dyn ProviderCatalog>) -> AgentBuilder {
        AgentBuilder::new(catalog)
    }

    // Start a mutable multi-turn session against the chosen provider.
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

    // Execute a single prompt and return the completed trajectory.
    pub fn completion(&self, message: &str, run: RunOptions) -> Result<Trajectory> {
        let mut session = self.start_session(run)?;
        session.send(message)?;
        session.end()
    }
}

// Read the provider fallback order from the environment.
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

// Read the requested model tier from the environment.
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
    // Rehydrate a saved trajectory into a writable session wrapper.
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

    // Borrow the current in-memory trajectory.
    pub fn trajectory(&self) -> &Trajectory {
        &self.trajectory
    }

    // Borrow the raw CLI output from the last provider call.
    pub fn last_raw_output(&self) -> Option<&str> {
        self.last_raw_output.as_deref()
    }

    // Send a new user message through the active provider session.
    pub fn send(&mut self, message: &str) -> Result<Turn> {
        let resume = self.build_resume_context();
        let prompt = if resume.is_some() {
            message.to_string()
        } else {
            render_replay_prompt(
                self.system_prompt.as_deref(),
                &self.trajectory.turns,
                message,
            )
        };
        let response = self.provider.execute(&ProviderRequest {
            session_id: self.trajectory.session_id.clone(),
            message: message.to_string(),
            prompt,
            model: self.model.clone(),
            model_tier: self.model_tier.clone(),
            reasoning: self.reasoning.clone(),
            system_prompt: self.system_prompt.clone(),
            cwd: self.cwd.clone(),
            mcp_servers: self.mcp_servers.clone(),
            resume,
        })?;
        merge_metadata(&mut self.trajectory.metadata, response.metadata);
        let output = if response.blocks.is_empty() {
            vec![ContentBlock::Text {
                text: response.text.clone(),
            }]
        } else {
            response.blocks.clone()
        };
        let turn = Turn {
            input: message.to_string(),
            output,
            usage: response.usage.clone(),
            duration_ms: response.duration_ms,
        };
        self.last_raw_output = Some(response.raw_output);
        if self.trajectory.model.is_empty() {
            if let Some(model) = self
                .trajectory
                .metadata
                .get("resolved_model")
                .and_then(Value::as_str)
            {
                self.trajectory.model = model.to_string();
            }
        }
        self.trajectory.append_turn(turn.clone());
        self.persist_turn(&turn)?;
        if let Some(path) = &self.traj_path {
            self.save_trajectory(path)?;
        }
        Ok(turn)
    }

    // Write the current trajectory snapshot to a specific path.
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

    // Finalize the session and flush the last trajectory snapshot to disk.
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

    // Return a self-contained handle that can reopen the backend session later.
    pub fn resume_handle(&self) -> String {
        serde_json::to_string(&ResumeHandleData {
            version: 1,
            provider: self.provider.name().to_string(),
            session_id: self.trajectory.session_id.clone(),
            provider_session_id: self.trajectory.metadata_string("provider_session_id"),
            provider_resume_key: self.trajectory.metadata_string("provider_resume_key"),
            model: self.model.clone(),
            reasoning: self.reasoning.clone(),
            system_prompt: self.system_prompt.clone(),
        })
        .unwrap_or_else(|_| self.trajectory.session_id.clone())
    }

    // Reload a stored session directly from a `data_dir` session folder.
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

    // Rebuild a session from a self-contained resume handle payload.
    pub fn from_resume_handle(
        provider: Arc<dyn crate::provider::Provider>,
        handle: &str,
        cwd: Option<PathBuf>,
    ) -> Result<Self> {
        let handle = parse_resume_handle(handle)?;
        let mut trajectory = Trajectory::new(
            handle.provider.clone(),
            handle.model.clone().unwrap_or_default(),
            handle.session_id.clone(),
        );
        trajectory.reasoning = handle.reasoning.clone().unwrap_or_default();
        trajectory.system_prompt = handle.system_prompt.clone().unwrap_or_default();
        if let Some(provider_session_id) = handle.provider_session_id.clone() {
            trajectory.metadata.insert(
                "provider_session_id".to_string(),
                serde_json::json!(provider_session_id),
            );
        }
        if let Some(provider_resume_key) = handle.provider_resume_key.clone() {
            trajectory.metadata.insert(
                "provider_resume_key".to_string(),
                serde_json::json!(provider_resume_key),
            );
        }
        Ok(Self {
            provider,
            cwd: cwd.unwrap_or(std::env::current_dir()?),
            model: handle.model,
            model_tier: None,
            reasoning: handle.reasoning,
            system_prompt: handle.system_prompt,
            mcp_servers: Vec::new(),
            data_dir: None,
            trajectory,
            traj_path: None,
            last_raw_output: None,
        })
    }

    // Append the initial metadata line for on-disk session storage.
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

    // Persist one completed turn into the JSONL log and turn files.
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

    // Append one JSON value as a line to the session event log.
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

    // Resolve the per-session storage directory under the configured data dir.
    fn session_root(&self) -> Option<PathBuf> {
        self.data_dir
            .as_ref()
            .map(|data_dir| data_dir.join("sessions").join(&self.trajectory.session_id))
    }

    // Build a provider-native resume descriptor from saved trajectory metadata.
    fn build_resume_context(&self) -> Option<ResumeContext> {
        if !self.provider.supports_native_resume() {
            return None;
        }
        let provider_session_id = self.trajectory.metadata_string("provider_session_id");
        let provider_resume_key = self.trajectory.metadata_string("provider_resume_key");
        if provider_session_id.is_none() && provider_resume_key.is_none() {
            return None;
        }
        Some(ResumeContext {
            session_id: self.trajectory.session_id.clone(),
            provider_session_id,
            provider_resume_key,
        })
    }
}

// Render a replayable prompt when provider-native resume is unavailable.
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

// Merge provider-returned metadata into the trajectory metadata map.
fn merge_metadata(
    metadata: &mut std::collections::BTreeMap<String, Value>,
    new_values: std::collections::BTreeMap<String, Value>,
) {
    for (key, value) in new_values {
        metadata.insert(key, value);
    }
}

// Parse a self-contained resume handle string into a typed payload.
fn parse_resume_handle(handle: &str) -> Result<ResumeHandleData> {
    Ok(serde_json::from_str(handle)?)
}

// Atomically replace a JSON file so readers never observe a partial write.
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
        // Return the provider name used by the mock catalog.
        fn name(&self) -> &'static str {
            "mock"
        }

        // Return the mock binary name for health checks.
        fn binary_name(&self) -> &'static str {
            "mock"
        }

        // Execute a mocked provider request and echo the assistant suffix.
        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
            Ok(ProviderResponse {
                text: format!(
                    "echo: {}",
                    request.prompt.lines().last().unwrap_or_default()
                ),
                blocks: Vec::new(),
                raw_output: "raw".to_string(),
                usage: UsageStats::default(),
                duration_ms: 0,
                metadata: BTreeMap::new(),
            })
        }

        // Pretend the provider binary exists on disk.
        fn find_binary(&self) -> Option<PathBuf> {
            Some(PathBuf::from("mock"))
        }
    }

    struct MockCatalog;

    impl ProviderCatalog for MockCatalog {
        // Resolve the mock provider by name.
        fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
            if name == "mock" {
                Some(Arc::new(MockProvider))
            } else {
                None
            }
        }

        // Return the list of registered mock providers.
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
        let session_dir = temp
            .path()
            .join("sessions")
            .join(session.trajectory().session_id.clone());
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

    struct ResumeAwareProvider {
        requests: Arc<Mutex<Vec<ProviderRequest>>>,
    }

    impl Provider for ResumeAwareProvider {
        // Return the provider name used by the resume-aware mock.
        fn name(&self) -> &'static str {
            "resume-mock"
        }

        // Return the binary name used by the resume-aware mock.
        fn binary_name(&self) -> &'static str {
            "resume-mock"
        }

        // Declare native resume support for the resume-aware mock provider.
        fn supports_native_resume(&self) -> bool {
            true
        }

        // Capture requests and emit a provider resume key after the first turn.
        fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
            self.requests.lock().unwrap().push(request.clone());
            let mut metadata = BTreeMap::new();
            metadata.insert(
                "provider_session_id".to_string(),
                serde_json::json!(request.session_id),
            );
            metadata.insert(
                "provider_resume_key".to_string(),
                serde_json::json!("thread-1"),
            );
            metadata.insert(
                "resolved_model".to_string(),
                serde_json::json!("mock-model"),
            );
            Ok(ProviderResponse {
                text: request.message.clone(),
                blocks: Vec::new(),
                raw_output: String::new(),
                usage: UsageStats::default(),
                duration_ms: 7,
                metadata,
            })
        }

        // Pretend the provider binary exists on disk.
        fn find_binary(&self) -> Option<PathBuf> {
            Some(PathBuf::from("resume-mock"))
        }
    }

    struct ResumeCatalog {
        provider: Arc<ResumeAwareProvider>,
    }

    impl ProviderCatalog for ResumeCatalog {
        // Resolve the single resume-aware mock provider by name.
        fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
            if name == "resume-mock" {
                Some(self.provider.clone())
            } else {
                None
            }
        }

        // Return the single registered resume-aware provider.
        fn providers(&self) -> Vec<Arc<dyn Provider>> {
            vec![self.provider.clone()]
        }
    }

    #[test]
    // Verify that provider-native resume is used after the first turn records a resume key.
    fn session_uses_native_resume_after_first_turn() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(ResumeAwareProvider {
            requests: requests.clone(),
        });
        let agent = Agent::builder(Arc::new(ResumeCatalog { provider }))
            .with_options(AgentOptions {
                provider: Some(vec!["resume-mock".to_string()]),
                ..Default::default()
            })
            .build();
        let mut session = agent.start_session(RunOptions::default()).unwrap();
        session.send("first turn").unwrap();
        session.send("second turn").unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].resume.is_none());
        assert_eq!(
            requests[1]
                .resume
                .as_ref()
                .and_then(|resume| resume.provider_resume_key.as_deref()),
            Some("thread-1")
        );
        assert_eq!(requests[1].prompt, "second turn");
    }

    #[test]
    // Verify that self-contained resume handles can be parsed and reused.
    fn session_can_be_rebuilt_from_resume_handle() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = Arc::new(ResumeAwareProvider {
            requests: requests.clone(),
        });
        let agent = Agent::builder(Arc::new(ResumeCatalog {
            provider: provider.clone(),
        }))
        .with_options(AgentOptions {
            provider: Some(vec!["resume-mock".to_string()]),
            ..Default::default()
        })
        .build();
        let mut session = agent.start_session(RunOptions::default()).unwrap();
        session.send("first turn").unwrap();
        let handle = session.resume_handle();
        let parsed = parse_resume_handle(&handle).unwrap();
        assert_eq!(parsed.provider, "resume-mock");
        let mut resumed = Session::from_resume_handle(provider, &handle, None).unwrap();
        resumed.send("second turn").unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[1]
                .resume
                .as_ref()
                .and_then(|resume| resume.provider_resume_key.as_deref()),
            Some("thread-1")
        );
    }
}
