use anyhow::{anyhow, Context, Result};
use daddy_core::{
    provider::{Provider, ProviderCatalog, ProviderRequest, ProviderResponse},
    AuthSignal, ModelTier,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

pub fn default_catalog() -> Arc<dyn ProviderCatalog> {
    Arc::new(StaticCatalog::new(vec![
        Arc::new(ClaudeProvider),
        Arc::new(CodexProvider),
        Arc::new(OpencodeProvider),
    ]))
}

struct StaticCatalog {
    providers: Vec<Arc<dyn Provider>>,
    aliases: BTreeMap<String, Arc<dyn Provider>>,
}

impl StaticCatalog {
    fn new(providers: Vec<Arc<dyn Provider>>) -> Self {
        let mut aliases = BTreeMap::new();
        for provider in &providers {
            aliases.insert(provider.name().to_string(), provider.clone());
            for alias in provider.aliases() {
                aliases.insert((*alias).to_string(), provider.clone());
            }
        }
        Self { providers, aliases }
    }
}

impl ProviderCatalog for StaticCatalog {
    fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.aliases.get(name).cloned()
    }

    fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.clone()
    }
}

struct CodexProvider;
struct ClaudeProvider;
struct OpencodeProvider;

impl Provider for CodexProvider {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["openai", "codex-cli"]
    }

    fn binary_name(&self) -> &'static str {
        "codex"
    }

    fn resolve_model(&self, tier: &ModelTier) -> Option<String> {
        Some(match tier {
            ModelTier::Strongest => "gpt-4.1".to_string(),
            ModelTier::Fast => "o4-mini".to_string(),
        })
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let temp = tempfile::NamedTempFile::new()?;
        let mut cmd = Command::new(self.binary_name());
        cmd.current_dir(&request.cwd)
            .arg("exec")
            .arg("--json")
            .arg("--output-last-message")
            .arg(temp.path());
        if let Some(model) = request.model.as_ref() {
            cmd.arg("--model").arg(model);
        }
        cmd.arg(&request.prompt);
        let output = cmd.output().context("failed to run codex CLI")?;
        if !output.status.success() {
            return Err(render_command_error("codex", &output));
        }
        let text = fs::read_to_string(temp.path()).unwrap_or_else(|_| String::new());
        Ok(ProviderResponse {
            text: if text.trim().is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                text.trim().to_string()
            },
            raw_output: String::from_utf8_lossy(&output.stdout).to_string(),
            usage: extract_codex_usage(&output.stdout),
        })
    }

    fn check_auth(&self) -> Option<AuthSignal> {
        env_or_file_auth(
            "OPENAI_API_KEY",
            &[home_path(".codex/auth.json"), home_path(".config/codex/auth.json")],
        )
    }
}

impl Provider for ClaudeProvider {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["claude_code", "cc"]
    }

    fn binary_name(&self) -> &'static str {
        "claude"
    }

    fn resolve_model(&self, tier: &ModelTier) -> Option<String> {
        Some(match tier {
            ModelTier::Strongest => "claude-opus-4-8".to_string(),
            ModelTier::Fast => "claude-sonnet-5".to_string(),
        })
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let mut cmd = Command::new(self.binary_name());
        cmd.current_dir(&request.cwd).arg("-p").arg("--output-format").arg("json");
        if let Some(model) = request.model.as_ref() {
            cmd.arg("--model").arg(model);
        }
        cmd.arg(&request.prompt);
        let output = cmd.output().context("failed to run claude CLI")?;
        if !output.status.success() {
            return Err(render_command_error("claude", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = extract_claude_text(&stdout).unwrap_or_else(|| stdout.trim().to_string());
        Ok(ProviderResponse {
            text,
            raw_output: stdout,
            usage: Default::default(),
        })
    }

    fn check_auth(&self) -> Option<AuthSignal> {
        env_or_file_auth("ANTHROPIC_API_KEY", &[home_path(".claude/.credentials.json")])
    }
}

impl Provider for OpencodeProvider {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn aliases(&self) -> &'static [&'static str] {
        &["open-code"]
    }

    fn binary_name(&self) -> &'static str {
        "opencode"
    }

    fn resolve_model(&self, tier: &ModelTier) -> Option<String> {
        Some(match tier {
            ModelTier::Strongest => "anthropic/claude-sonnet-5".to_string(),
            ModelTier::Fast => "openai/gpt-4.1-mini".to_string(),
        })
    }

    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let mut cmd = Command::new(self.binary_name());
        cmd.current_dir(&request.cwd).arg("run").arg("--format").arg("json");
        if let Some(model) = request.model.as_ref() {
            cmd.arg("--model").arg(model);
        }
        if let Some(reasoning) = request.reasoning.as_ref() {
            cmd.arg("--variant").arg(reasoning);
        }
        cmd.arg(&request.prompt);
        let output = cmd.output().context("failed to run opencode CLI")?;
        if !output.status.success() {
            return Err(render_command_error("opencode", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = extract_opencode_text(&stdout).unwrap_or_else(|| stdout.trim().to_string());
        Ok(ProviderResponse {
            text,
            raw_output: stdout,
            usage: Default::default(),
        })
    }

    fn check_auth(&self) -> Option<AuthSignal> {
        env_or_file_auth(
            "OPENCODE_API_KEY",
            &[home_path(".config/opencode/config.json"), home_path(".config/opencode/auth.json")],
        )
    }
}

fn render_command_error(name: &str, output: &std::process::Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    anyhow!("{name} execution failed: {}", if !stderr.is_empty() { stderr } else { stdout })
}

fn env_or_file_auth(env_var: &str, files: &[PathBuf]) -> Option<AuthSignal> {
    if std::env::var_os(env_var).is_some() {
        return Some(AuthSignal {
            present: true,
            detail: format!("${env_var} set"),
            credentials_path: None,
        });
    }
    let file = files.iter().find(|path| path.exists())?;
    Some(AuthSignal {
        present: true,
        detail: format!("credentials file found at {}", file.display()),
        credentials_path: Some(file.display().to_string()),
    })
}

fn home_path(relative: &str) -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(relative)
}

fn extract_codex_usage(stdout: &[u8]) -> daddy_core::UsageStats {
    let mut usage = daddy_core::UsageStats::default();
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let Some(event) = obj.get("event").and_then(Value::as_str) else {
            continue;
        };
        if event == "turn.completed" || event == "response.completed" {
            if let Some(tokens) = obj.get("usage").and_then(Value::as_object) {
                usage.input_tokens = tokens.get("input_tokens").and_then(Value::as_u64).unwrap_or(0);
                usage.output_tokens = tokens.get("output_tokens").and_then(Value::as_u64).unwrap_or(0);
            }
            if let Some(cost) = obj.get("cost_usd").and_then(Value::as_f64) {
                usage.cost_usd = cost;
            }
        }
    }
    usage
}

fn extract_claude_text(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    value
        .get("result")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| value.get("content").and_then(Value::as_str).map(ToString::to_string))
}

fn extract_opencode_text(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    value
        .get("text")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| value.get("content").and_then(Value::as_str).map(ToString::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_resolves_aliases() {
        let catalog = default_catalog();
        assert_eq!(catalog.get("claude_code").unwrap().name(), "claude");
        assert_eq!(catalog.get("codex").unwrap().name(), "codex");
    }

    #[test]
    fn usage_parser_handles_jsonl() {
        let input = br#"{"event":"turn.completed","usage":{"input_tokens":12,"output_tokens":4},"cost_usd":0.02}"#;
        let usage = extract_codex_usage(input);
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 4);
    }
}
