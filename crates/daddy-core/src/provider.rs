use crate::models::{AuthSignal, MCPServer, ModelTier, ProviderHealth, UsageStats};
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub prompt: String,
    pub model: Option<String>,
    pub model_tier: Option<ModelTier>,
    pub reasoning: Option<String>,
    pub system_prompt: Option<String>,
    pub cwd: PathBuf,
    pub mcp_servers: Vec<MCPServer>,
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub text: String,
    pub raw_output: String,
    pub usage: UsageStats,
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }
    fn binary_name(&self) -> &'static str;
    fn resolve_model(&self, _tier: &ModelTier) -> Option<String> {
        None
    }
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse>;
    fn check_auth(&self) -> Option<AuthSignal> {
        None
    }
    fn find_binary(&self) -> Option<PathBuf> {
        which::which(self.binary_name()).ok()
    }
    fn check_health(&self) -> ProviderHealth {
        ProviderHealth {
            provider: self.name().to_string(),
            installed: self.find_binary().is_some(),
            binary_path: self.find_binary().map(|path| path.display().to_string()),
            auth: self.check_auth(),
        }
    }
}

pub trait ProviderCatalog: Send + Sync {
    fn get(&self, name: &str) -> Option<Arc<dyn Provider>>;
    fn providers(&self) -> Vec<Arc<dyn Provider>>;

    fn first_installed(&self, order: &[String]) -> Option<Arc<dyn Provider>> {
        for name in order {
            if let Some(provider) = self.get(name) {
                if provider.find_binary().is_some() {
                    return Some(provider);
                }
            }
        }
        order.first().and_then(|name| self.get(name))
    }
}

pub fn resolve_cwd(cwd: Option<&Path>) -> Result<PathBuf> {
    Ok(match cwd {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    })
}
