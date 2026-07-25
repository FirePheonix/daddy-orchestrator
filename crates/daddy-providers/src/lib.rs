use anyhow::{Context, Result, anyhow};
use daddy_core::{
    AuthSignal, MCPServer, ModelTier,
    provider::{Provider, ProviderCatalog, ProviderRequest, ProviderResponse},
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

// Build the default provider catalog used by the CLI and tests.
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
    // Index providers by canonical name and alias for fast lookup.
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
    // Resolve a provider by canonical name or alias.
    fn get(&self, name: &str) -> Option<Arc<dyn Provider>> {
        self.aliases.get(name).cloned()
    }

    // Return all registered providers in insertion order.
    fn providers(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.clone()
    }
}

struct CodexProvider;
struct ClaudeProvider;
struct OpencodeProvider;

impl Provider for CodexProvider {
    // Return the canonical provider name.
    fn name(&self) -> &'static str {
        "codex"
    }

    // Return alternate names that map to the codex provider.
    fn aliases(&self) -> &'static [&'static str] {
        &["openai", "codex-cli"]
    }

    // Return the codex CLI executable name.
    fn binary_name(&self) -> &'static str {
        "codex"
    }

    // Map model tiers onto concrete codex model names.
    fn resolve_model(&self, tier: &ModelTier) -> Option<String> {
        Some(match tier {
            ModelTier::Strongest => "gpt-4.1".to_string(),
            ModelTier::Fast => "o4-mini".to_string(),
        })
    }

    // Advertise that codex can resume prior backend sessions.
    fn supports_native_resume(&self) -> bool {
        true
    }

    // Execute a codex request and capture usage plus resume metadata.
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let temp = tempfile::NamedTempFile::new()?;
        let mut cmd = Command::new(self.binary_name());
        cmd.current_dir(&request.cwd).arg("exec");
        if let Some(resume_key) = request
            .resume
            .as_ref()
            .and_then(|resume| resume.provider_resume_key.as_deref())
        {
            cmd.arg("resume").arg(resume_key);
        }
        cmd.arg("--json")
            .arg("--skip-git-repo-check")
            .arg("--output-last-message")
            .arg(temp.path());
        if let Some(model) = request.model.as_ref() {
            cmd.arg("--model").arg(model);
        }
        if let Some(reasoning) = request.reasoning.as_ref() {
            cmd.arg("-c")
                .arg(format!("model_reasoning_effort=\"{reasoning}\""));
        }
        for arg in build_codex_mcp_args(&request.mcp_servers) {
            cmd.arg(arg);
        }
        cmd.arg(select_prompt(request));
        let output = cmd.output().context("failed to run codex CLI")?;
        if !output.status.success() {
            return Err(render_command_error("codex", &output));
        }
        let text = fs::read_to_string(temp.path()).unwrap_or_else(|_| String::new());
        let mut metadata = BTreeMap::new();
        if let Some(thread_id) = extract_codex_thread_id(&output.stdout) {
            metadata.insert(
                "provider_resume_key".to_string(),
                serde_json::json!(thread_id),
            );
        }
        if let Some(model) = request.model.clone() {
            metadata.insert("resolved_model".to_string(), serde_json::json!(model));
        }
        Ok(ProviderResponse {
            text: if text.trim().is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                text.trim().to_string()
            },
            raw_output: String::from_utf8_lossy(&output.stdout).to_string(),
            usage: extract_codex_usage(&output.stdout),
            duration_ms: 0,
            metadata,
        })
    }

    // Detect codex auth via env vars or local credential files.
    fn check_auth(&self) -> Option<AuthSignal> {
        env_or_file_auth(
            "OPENAI_API_KEY",
            &[
                home_path(".codex/auth.json"),
                home_path(".config/codex/auth.json"),
            ],
        )
    }
}

impl Provider for ClaudeProvider {
    // Return the canonical provider name.
    fn name(&self) -> &'static str {
        "claude"
    }

    // Return alternate names that map to the claude provider.
    fn aliases(&self) -> &'static [&'static str] {
        &["claude_code", "cc"]
    }

    // Return the claude CLI executable name.
    fn binary_name(&self) -> &'static str {
        "claude"
    }

    // Map model tiers onto concrete Claude model names.
    fn resolve_model(&self, tier: &ModelTier) -> Option<String> {
        Some(match tier {
            ModelTier::Strongest => "claude-opus-4-8".to_string(),
            ModelTier::Fast => "claude-sonnet-5".to_string(),
        })
    }

    // Advertise that claude can resume prior backend sessions.
    fn supports_native_resume(&self) -> bool {
        true
    }

    // Execute a claude request and preserve its reusable session id.
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let mut cmd = Command::new(self.binary_name());
        let mcp_config = build_claude_mcp_config(&request.mcp_servers)?;
        cmd.current_dir(&request.cwd)
            .arg("-p")
            .arg("--output-format")
            .arg("json");
        if let Some(resume) = request.resume.as_ref() {
            cmd.arg("--resume").arg(
                resume
                    .provider_session_id
                    .as_deref()
                    .unwrap_or(&resume.session_id),
            );
        } else {
            cmd.arg("--session-id").arg(&request.session_id);
            if let Some(system_prompt) = request.system_prompt.as_ref() {
                cmd.arg("--system-prompt").arg(system_prompt);
            }
        }
        if let Some(model) = request.model.as_ref() {
            cmd.arg("--model").arg(model);
        }
        if let Some(reasoning) = request.reasoning.as_ref() {
            cmd.arg("--effort").arg(reasoning);
        }
        if let Some(mcp_config) = mcp_config.as_ref() {
            cmd.arg("--mcp-config").arg(mcp_config.path());
        }
        cmd.arg(select_prompt(request));
        let output = cmd.output().context("failed to run claude CLI")?;
        if !output.status.success() {
            return Err(render_command_error("claude", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = extract_claude_text(&stdout).unwrap_or_else(|| stdout.trim().to_string());
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "provider_session_id".to_string(),
            serde_json::json!(request.session_id),
        );
        metadata.insert(
            "provider_resume_key".to_string(),
            serde_json::json!(request.session_id),
        );
        if let Some(model) = extract_claude_model(&stdout).or_else(|| request.model.clone()) {
            metadata.insert("resolved_model".to_string(), serde_json::json!(model));
        }
        Ok(ProviderResponse {
            text,
            raw_output: stdout,
            usage: extract_claude_usage(&output.stdout),
            duration_ms: extract_claude_duration(&output.stdout),
            metadata,
        })
    }

    // Detect claude auth via env vars or local credential files.
    fn check_auth(&self) -> Option<AuthSignal> {
        env_or_file_auth(
            "ANTHROPIC_API_KEY",
            &[home_path(".claude/.credentials.json")],
        )
    }
}

impl Provider for OpencodeProvider {
    // Return the canonical provider name.
    fn name(&self) -> &'static str {
        "opencode"
    }

    // Return alternate names that map to the opencode provider.
    fn aliases(&self) -> &'static [&'static str] {
        &["open-code"]
    }

    // Return the opencode CLI executable name.
    fn binary_name(&self) -> &'static str {
        "opencode"
    }

    // Map model tiers onto concrete opencode model names.
    fn resolve_model(&self, tier: &ModelTier) -> Option<String> {
        Some(match tier {
            ModelTier::Strongest => "anthropic/claude-sonnet-5".to_string(),
            ModelTier::Fast => "openai/gpt-4.1-mini".to_string(),
        })
    }

    // Advertise that opencode can resume prior backend sessions.
    fn supports_native_resume(&self) -> bool {
        true
    }

    // Execute an opencode request and capture the emitted session id.
    fn execute(&self, request: &ProviderRequest) -> Result<ProviderResponse> {
        let config = build_opencode_config(&request.mcp_servers)?;
        let mut cmd = Command::new(self.binary_name());
        cmd.current_dir(&request.cwd)
            .arg("run")
            .arg("--format")
            .arg("json");
        if let Some(resume_key) = request
            .resume
            .as_ref()
            .and_then(|resume| resume.provider_resume_key.as_deref())
        {
            cmd.arg("--session").arg(resume_key);
        }
        if let Some(model) = request.model.as_ref() {
            cmd.arg("--model").arg(model);
        }
        if let Some(reasoning) = request.reasoning.as_ref() {
            cmd.arg("--variant").arg(reasoning);
        }
        cmd.arg("--thinking").arg(select_prompt(request));
        if let Some(config) = config.as_ref() {
            cmd.env("OPENCODE_CONFIG", config.path());
        }
        let output = cmd.output().context("failed to run opencode CLI")?;
        if !output.status.success() {
            return Err(render_command_error("opencode", &output));
        }
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let text = extract_opencode_text(&stdout).unwrap_or_else(|| stdout.trim().to_string());
        let mut metadata = BTreeMap::new();
        if let Some(session_id) = extract_opencode_session_id(&output.stdout) {
            metadata.insert(
                "provider_resume_key".to_string(),
                serde_json::json!(session_id),
            );
        }
        if let Some(model) = request.model.clone() {
            metadata.insert("resolved_model".to_string(), serde_json::json!(model));
        }
        Ok(ProviderResponse {
            text,
            raw_output: stdout,
            usage: extract_opencode_usage(&output.stdout),
            duration_ms: 0,
            metadata,
        })
    }

    // Detect opencode auth via env vars or local credential files.
    fn check_auth(&self) -> Option<AuthSignal> {
        env_or_file_auth(
            "OPENCODE_API_KEY",
            &[
                home_path(".config/opencode/config.json"),
                home_path(".config/opencode/auth.json"),
            ],
        )
    }
}

// Select the message-only prompt for native resume and replay otherwise.
fn select_prompt(request: &ProviderRequest) -> &str {
    if request.resume.is_some() {
        &request.message
    } else {
        &request.prompt
    }
}

// Build codex CLI `-c` overrides for configured MCP servers.
fn build_codex_mcp_args(mcp_servers: &[MCPServer]) -> Vec<String> {
    let mut args = Vec::new();
    for server in mcp_servers {
        if !server.url.is_empty() {
            args.push("-c".to_string());
            args.push(format!(
                "mcp_servers.{}.url=\"{}\"",
                server.name, server.url
            ));
        } else if !server.command.is_empty() {
            args.push("-c".to_string());
            args.push(format!(
                "mcp_servers.{}.command=\"{}\"",
                server.name, server.command
            ));
            if !server.args.is_empty() {
                args.push("-c".to_string());
                args.push(format!(
                    "mcp_servers.{}.args={}",
                    server.name,
                    serde_json::to_string(&server.args).unwrap_or_else(|_| "[]".to_string())
                ));
            }
        }
    }
    args
}

// Write a temporary Claude MCP config file when remote or local servers are configured.
fn build_claude_mcp_config(mcp_servers: &[MCPServer]) -> Result<Option<tempfile::NamedTempFile>> {
    if mcp_servers.is_empty() {
        return Ok(None);
    }
    let mut file = tempfile::NamedTempFile::new()?;
    let mut servers = serde_json::Map::new();
    for server in mcp_servers {
        let mut entry = serde_json::Map::new();
        if !server.url.is_empty() {
            entry.insert("type".to_string(), serde_json::json!("http"));
            entry.insert("url".to_string(), serde_json::json!(server.url));
        } else {
            entry.insert("command".to_string(), serde_json::json!(server.command));
            entry.insert("args".to_string(), serde_json::json!(server.args));
            if !server.env.is_empty() {
                entry.insert("env".to_string(), serde_json::json!(server.env));
            }
        }
        servers.insert(server.name.clone(), Value::Object(entry));
    }
    serde_json::to_writer(&mut file, &serde_json::json!({ "mcpServers": servers }))?;
    file.write_all(b"\n")?;
    Ok(Some(file))
}

// Write a temporary opencode config file when remote or local MCP servers are configured.
fn build_opencode_config(mcp_servers: &[MCPServer]) -> Result<Option<tempfile::NamedTempFile>> {
    if mcp_servers.is_empty() {
        return Ok(None);
    }
    let mut file = tempfile::NamedTempFile::new()?;
    let mut mcp = serde_json::Map::new();
    for server in mcp_servers {
        let mut entry = serde_json::Map::new();
        if !server.url.is_empty() {
            entry.insert("type".to_string(), serde_json::json!("remote"));
            entry.insert("url".to_string(), serde_json::json!(server.url));
        } else {
            entry.insert("type".to_string(), serde_json::json!("local"));
            entry.insert(
                "command".to_string(),
                serde_json::json!(
                    std::iter::once(server.command.clone())
                        .chain(server.args.clone())
                        .collect::<Vec<_>>()
                ),
            );
            if !server.env.is_empty() {
                entry.insert("environment".to_string(), serde_json::json!(server.env));
            }
        }
        mcp.insert(server.name.clone(), Value::Object(entry));
    }
    serde_json::to_writer(
        &mut file,
        &serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "mcp": mcp
        }),
    )?;
    file.write_all(b"\n")?;
    Ok(Some(file))
}

// Extract the codex thread id from JSONL stdout.
fn extract_codex_thread_id(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("thread.started") {
            if let Some(thread_id) = value.get("thread_id").and_then(Value::as_str) {
                return Some(thread_id.to_string());
            }
        }
    }
    None
}

// Build an actionable command error from stdout and stderr.
fn render_command_error(name: &str, output: &std::process::Output) -> anyhow::Error {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    anyhow!(
        "{name} execution failed: {}",
        if !stderr.is_empty() { stderr } else { stdout }
    )
}

// Detect credentials from an env var or any known provider auth file.
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

// Resolve a path relative to the current user's home directory.
fn home_path(relative: &str) -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(relative)
}

// Parse codex token usage out of its JSONL stdout stream.
fn extract_codex_usage(stdout: &[u8]) -> daddy_core::UsageStats {
    let mut usage = daddy_core::UsageStats::default();
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(obj) = value.as_object() else {
            continue;
        };
        let event_type = obj
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| obj.get("event").and_then(Value::as_str));
        if event_type == Some("turn.completed") || event_type == Some("response.completed") {
            if let Some(tokens) = obj.get("usage").and_then(Value::as_object) {
                usage.input_tokens = tokens
                    .get("input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                usage.output_tokens = tokens
                    .get("output_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                usage.cache_read_tokens = tokens
                    .get("cached_input_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
            }
            if let Some(cost) = obj.get("cost_usd").and_then(Value::as_f64) {
                usage.cost_usd = cost;
            }
        }
    }
    usage
}

// Extract the final text field from Claude JSON output.
fn extract_claude_text(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    value
        .get("result")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("content")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

// Extract usage fields from Claude JSON output when present.
fn extract_claude_usage(stdout: &[u8]) -> daddy_core::UsageStats {
    let value: Value = match serde_json::from_slice(stdout) {
        Ok(value) => value,
        Err(_) => return daddy_core::UsageStats::default(),
    };
    let Some(usage) = value.get("usage").and_then(Value::as_object) else {
        return daddy_core::UsageStats::default();
    };
    daddy_core::UsageStats {
        input_tokens: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output_tokens: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_read_tokens: usage
            .get("cache_read_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_write_tokens: usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cost_usd: value
            .get("total_cost_usd")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
    }
}

// Extract the resolved Claude model from JSON output when available.
fn extract_claude_model(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    value
        .get("model")
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

// Extract the total duration from Claude JSON output when available.
fn extract_claude_duration(stdout: &[u8]) -> u128 {
    let value: Value = match serde_json::from_slice(stdout) {
        Ok(value) => value,
        Err(_) => return 0,
    };
    value
        .get("duration_ms")
        .and_then(Value::as_u64)
        .map(u128::from)
        .unwrap_or(0)
}

// Extract the final text field from opencode JSON output.
fn extract_opencode_text(stdout: &str) -> Option<String> {
    let value: Value = serde_json::from_str(stdout).ok()?;
    value
        .get("text")
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| {
            value
                .get("content")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        })
}

// Extract opencode usage from its JSON event stream.
fn extract_opencode_usage(stdout: &[u8]) -> daddy_core::UsageStats {
    let mut usage = daddy_core::UsageStats::default();
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("step_finish") {
            continue;
        }
        let Some(part) = value.get("part").and_then(Value::as_object) else {
            continue;
        };
        let tokens = part.get("tokens").and_then(Value::as_object);
        let cache = tokens
            .and_then(|tokens| tokens.get("cache"))
            .and_then(Value::as_object);
        usage.input_tokens += tokens
            .and_then(|tokens| tokens.get("input"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage.output_tokens += tokens
            .and_then(|tokens| tokens.get("output"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage.output_tokens += tokens
            .and_then(|tokens| tokens.get("reasoning"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage.cache_read_tokens += cache
            .and_then(|cache| cache.get("read"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage.cache_write_tokens += cache
            .and_then(|cache| cache.get("write"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        usage.cost_usd += part.get("cost").and_then(Value::as_f64).unwrap_or(0.0);
    }
    usage
}

// Extract opencode's backend session id from its JSON event stream.
fn extract_opencode_session_id(stdout: &[u8]) -> Option<String> {
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(session_id) = value.get("sessionID").and_then(Value::as_str) {
            if !session_id.is_empty() {
                return Some(session_id.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Confirm aliases resolve back to their canonical provider.
    fn catalog_resolves_aliases() {
        let catalog = default_catalog();
        assert_eq!(catalog.get("claude_code").unwrap().name(), "claude");
        assert_eq!(catalog.get("codex").unwrap().name(), "codex");
    }

    #[test]
    // Confirm codex usage parsing accepts JSONL events with token counts.
    fn usage_parser_handles_jsonl() {
        let input = br#"{"type":"turn.completed","usage":{"input_tokens":12,"output_tokens":4},"cost_usd":0.02}"#;
        let usage = extract_codex_usage(input);
        assert_eq!(usage.input_tokens, 12);
        assert_eq!(usage.output_tokens, 4);
    }

    #[test]
    // Confirm codex resume metadata is extracted from the JSONL event stream.
    fn codex_thread_id_is_extracted() {
        let input = br#"{"type":"thread.started","thread_id":"thread-123"}"#;
        assert_eq!(
            extract_codex_thread_id(input).as_deref(),
            Some("thread-123")
        );
    }

    #[test]
    // Confirm opencode resume metadata is extracted from the JSONL event stream.
    fn opencode_session_id_is_extracted() {
        let input = br#"{"type":"step_start","sessionID":"session-123"}"#;
        assert_eq!(
            extract_opencode_session_id(input).as_deref(),
            Some("session-123")
        );
    }
}
