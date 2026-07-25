use crate::models::{MCPServer, ModelTier};
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct AgentOptions {
    pub provider: Option<Vec<String>>,
    pub model: Option<String>,
    pub model_tier: Option<ModelTier>,
    pub reasoning: Option<String>,
    pub system_prompt: Option<String>,
    pub data_dir: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    pub metadata: std::collections::BTreeMap<String, serde_json::Value>,
    pub mcp_servers: Vec<MCPServer>,
}

#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub traj_path: Option<PathBuf>,
}
