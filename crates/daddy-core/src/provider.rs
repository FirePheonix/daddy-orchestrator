use crate::models::{AuthSignal, ContentBlock, MCPServer, ModelTier, ProviderHealth, UsageStats};
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct ResumeContext {
    pub session_id: String,
    pub provider_session_id: Option<String>,
    pub provider_resume_key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProviderRequest {
    pub session_id: String,
    pub message: String,
    pub prompt: String,
    pub model: Option<String>,
    pub model_tier: Option<ModelTier>,
    pub reasoning: Option<String>,
    pub system_prompt: Option<String>,
    pub cwd: PathBuf,
    pub mcp_servers: Vec<MCPServer>,
    pub resume: Option<ResumeContext>,
}

#[derive(Debug, Clone)]
pub struct ProviderResponse {
    pub text: String,
    pub blocks: Vec<ContentBlock>,
    pub raw_output: String,
    pub usage: UsageStats,
    pub duration_ms: u128,
    pub metadata: BTreeMap<String, Value>,
}

pub trait Provider: Send + Sync {
    fn name(&self) -> &'static str;
    // Return alias names that map to the same provider implementation.
    fn aliases(&self) -> &'static [&'static str] {
        &[]
    }
    fn binary_name(&self) -> &'static str;
    // Map an abstract tier into a provider-specific model identifier.
    fn resolve_model(&self, _tier: &ModelTier) -> Option<String> {
        None
    }
    // Indicate whether the provider can continue a prior backend session natively.
    fn supports_native_resume(&self) -> bool {
        false
    }
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse>;
    // Detect a provider-specific usage-limit message and return the wait time in minutes.
    fn usage_limit_wait(&self, _text: &str) -> Option<u32> {
        None
    }
    // Detect auth state through cheap local signals such as env vars or files.
    fn check_auth(&self) -> Option<AuthSignal> {
        None
    }
    // Locate the provider CLI on the current machine.
    fn find_binary(&self) -> Option<PathBuf> {
        which::which(self.binary_name()).ok()
    }
    // Build a health summary for the provider binary and local auth state.
    fn check_health(&self) -> ProviderHealth {
        ProviderHealth {
            provider: self.name().to_string(),
            installed: self.find_binary().is_some(),
            binary_path: self.find_binary().map(|path| path.display().to_string()),
            auth: self.check_auth(),
            probed: false,
            rate_limited: None,
            wait_minutes: None,
            error: None,
        }
    }

    // Probe the provider with a minimal prompt to confirm responsiveness or detect active limits.
    fn check_health_live(&self, model: Option<&str>) -> ProviderHealth {
        let mut health = self.check_health();
        if !health.installed {
            return health;
        }
        health.probed = true;
        match self.live_probe(model) {
            Ok(wait) => {
                health.rate_limited = Some(wait.is_some());
                health.wait_minutes = wait;
            }
            Err(error) => {
                health.error = Some(error.to_string());
            }
        }
        health
    }

    // Run a low-cost prompt and classify the response as healthy or rate-limited.
    fn live_probe(&self, model: Option<&str>) -> Result<Option<u32>> {
        let request = ProviderRequest {
            session_id: "doctor-probe".to_string(),
            message: "Reply with ok.".to_string(),
            prompt: "Reply with ok.".to_string(),
            model: model.map(ToString::to_string),
            model_tier: None,
            reasoning: None,
            system_prompt: None,
            cwd: std::env::current_dir()?,
            mcp_servers: Vec::new(),
            resume: None,
        };
        let response = self.execute(&request)?;
        Ok(self
            .usage_limit_wait(&response.text)
            .or_else(|| self.usage_limit_wait(&response.raw_output)))
    }
}

pub trait ProviderCatalog: Send + Sync {
    fn get(&self, name: &str) -> Option<Arc<dyn Provider>>;
    fn providers(&self) -> Vec<Arc<dyn Provider>>;

    // Pick the first installed provider from an ordered fallback list.
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

// Resolve the working directory for provider subprocess execution.
pub fn resolve_cwd(cwd: Option<&Path>) -> Result<PathBuf> {
    Ok(match cwd {
        Some(path) => path.to_path_buf(),
        None => std::env::current_dir()?,
    })
}
