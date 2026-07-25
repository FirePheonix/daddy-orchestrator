use anyhow::{Context, Result, anyhow};
use daddy_core::{
    AuthSignal, ContentBlock, MCPServer, ModelTier, ToolCall, compute_cost,
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
        let temp = temporary_output_path()?;
        let mut cmd = provider_command(self);
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
            .arg(temp.as_ref() as &std::path::Path);
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
        let text =
            fs::read_to_string(temp.as_ref() as &std::path::Path).unwrap_or_else(|_| String::new());
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
        let mut usage = extract_codex_usage(&output.stdout);
        if usage.cost_usd == 0.0 {
            if let Some(model) = request.model.as_deref() {
                if let Some(cost) = compute_cost("codex", model, &usage) {
                    usage.cost_usd = cost;
                }
            }
        }
        Ok(ProviderResponse {
            text: if text.trim().is_empty() {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            } else {
                text.trim().to_string()
            },
            blocks: parse_codex_blocks(&output.stdout),
            raw_output: String::from_utf8_lossy(&output.stdout).to_string(),
            usage,
            duration_ms: 0,
            metadata,
        })
    }

    // Detect codex usage-limit messages from response text or stderr output.
    fn usage_limit_wait(&self, text: &str) -> Option<u32> {
        detect_codex_usage_limit(text)
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
        let mut cmd = provider_command(self);
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
        let mut usage = extract_claude_usage(&output.stdout);
        if usage.cost_usd == 0.0 {
            if let Some(model) = extract_claude_model(&stdout).or_else(|| request.model.clone()) {
                if let Some(cost) = compute_cost("claude", &model, &usage) {
                    usage.cost_usd = cost;
                }
            }
        }
        Ok(ProviderResponse {
            text,
            blocks: parse_claude_blocks(&stdout),
            raw_output: stdout,
            usage,
            duration_ms: extract_claude_duration(&output.stdout),
            metadata,
        })
    }

    // Detect Claude usage-limit messages from response text or stderr output.
    fn usage_limit_wait(&self, text: &str) -> Option<u32> {
        detect_claude_usage_limit(text)
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
        let mut cmd = provider_command(self);
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
        let mut usage = extract_opencode_usage(&output.stdout);
        if usage.cost_usd == 0.0 {
            if let Some(model) = request.model.as_deref() {
                if let Some(cost) = compute_cost("opencode", model, &usage) {
                    usage.cost_usd = cost;
                }
            }
        }
        Ok(ProviderResponse {
            text,
            blocks: parse_opencode_blocks(&output.stdout),
            raw_output: stdout,
            usage,
            duration_ms: 0,
            metadata,
        })
    }

    // Detect opencode usage-limit messages from response text or stderr output.
    fn usage_limit_wait(&self, text: &str) -> Option<u32> {
        detect_opencode_usage_limit(text)
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

// Build a process command from the resolved provider binary path when available.
fn provider_command(provider: &dyn Provider) -> Command {
    if let Some(path) = provider.find_binary() {
        if cfg!(windows)
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"))
                .unwrap_or(false)
        {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(path);
            cmd
        } else {
            Command::new(path)
        }
    } else {
        Command::new(provider.binary_name())
    }
}

// Create a temporary output path that external CLIs can write without an open file handle.
fn temporary_output_path() -> Result<tempfile::TempPath> {
    Ok(tempfile::NamedTempFile::new()?.into_temp_path())
}

// Detect a codex usage-limit message and estimate a retry wait time.
fn detect_codex_usage_limit(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if !lower.contains("usage limit") {
        return None;
    }
    extract_retry_minutes(text).or(Some(60))
}

// Detect a Claude usage-limit message and estimate a retry wait time.
fn detect_claude_usage_limit(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if !lower.contains("limit") && !lower.contains("out of usage") {
        return None;
    }
    if !lower.contains("reset") && !lower.contains("resets") {
        return None;
    }
    extract_retry_minutes(text).or(Some(60))
}

// Detect an opencode usage-limit message and estimate a retry wait time.
fn detect_opencode_usage_limit(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if !lower.contains("usagelimit") && !lower.contains("usage limit") {
        return None;
    }
    extract_retry_seconds(text)
        .map(|seconds| ((seconds / 60) + 1).max(1))
        .or(Some(60))
}

// Extract the first integer minutes count from limit-related text.
fn extract_retry_minutes(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if let Some(index) = lower.find("retry after") {
        let suffix = &lower[index + "retry after".len()..];
        return leading_number(suffix);
    }
    if let Some(index) = lower.find("wait") {
        let suffix = &lower[index + "wait".len()..];
        return leading_number(suffix);
    }
    None
}

// Extract the first integer seconds count from limit-related text.
fn extract_retry_seconds(text: &str) -> Option<u32> {
    let lower = text.to_lowercase();
    if let Some(index) = lower.find("retry after") {
        let suffix = &lower[index + "retry after".len()..];
        return leading_number(suffix);
    }
    None
}

// Parse the first unsigned integer that appears inside a string slice.
fn leading_number(text: &str) -> Option<u32> {
    let digits: String = text
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(|ch| ch.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
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

// Parse codex JSONL events into ordered content and tool-use blocks.
fn parse_codex_blocks(stdout: &[u8]) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let mut tool_positions = BTreeMap::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let event_type = value
            .get("type")
            .and_then(Value::as_str)
            .or_else(|| value.get("event").and_then(Value::as_str));
        match event_type {
            Some("item.started") => {
                handle_codex_item_started(&value, &mut blocks, &mut tool_positions)
            }
            Some("item.updated") | Some("item.completed") => {
                handle_codex_item_completed(&value, &mut blocks, &tool_positions)
            }
            Some("turn.failed") | Some("error") => handle_codex_error(&value, &mut blocks),
            _ => {}
        }
    }
    blocks
}

// Insert a codex tool block when the stream announces a new item.
fn handle_codex_item_started(
    value: &Value,
    blocks: &mut Vec<ContentBlock>,
    tool_positions: &mut BTreeMap<String, usize>,
) {
    let Some(item) = value.get("item").and_then(Value::as_object) else {
        return;
    };
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    let tool_id = item
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    match item_type {
        "command_execution" => {
            let index = push_tool_block(
                blocks,
                ToolCall {
                    id: tool_id.clone(),
                    name: "command_execution".to_string(),
                    arguments: serde_json::json!({ "command": item.get("command").and_then(Value::as_str).unwrap_or_default() }),
                    output: String::new(),
                    is_error: false,
                },
            );
            tool_positions.insert(tool_id, index);
        }
        "mcp_tool_call" => {
            let name = match (
                item.get("server").and_then(Value::as_str),
                item.get("tool").and_then(Value::as_str),
            ) {
                (Some(server), Some(tool)) if !server.is_empty() => format!("{server}.{tool}"),
                (_, Some(tool)) => tool.to_string(),
                _ => "mcp_tool_call".to_string(),
            };
            let arguments = item
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let index = push_tool_block(
                blocks,
                ToolCall {
                    id: tool_id.clone(),
                    name,
                    arguments,
                    output: String::new(),
                    is_error: false,
                },
            );
            tool_positions.insert(tool_id, index);
        }
        "file_change" => {
            let index = push_tool_block(
                blocks,
                ToolCall {
                    id: tool_id.clone(),
                    name: "file_change".to_string(),
                    arguments: serde_json::json!({
                        "file": item.get("file").and_then(Value::as_str).unwrap_or_default(),
                        "action": item.get("action").and_then(Value::as_str).unwrap_or_default()
                    }),
                    output: String::new(),
                    is_error: false,
                },
            );
            tool_positions.insert(tool_id, index);
        }
        _ => {}
    }
}

// Update a previously inserted codex block from the latest item payload.
fn handle_codex_item_completed(
    value: &Value,
    blocks: &mut Vec<ContentBlock>,
    tool_positions: &BTreeMap<String, usize>,
) {
    let Some(item) = value.get("item").and_then(Value::as_object) else {
        return;
    };
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
    match item_type {
        "reasoning" => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    blocks.push(ContentBlock::Thinking {
                        text: text.to_string(),
                    });
                }
            }
        }
        "agent_message" => {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                if !text.is_empty() {
                    blocks.push(ContentBlock::Text {
                        text: text.to_string(),
                    });
                }
            }
        }
        "command_execution" | "mcp_tool_call" | "file_change" => {
            let tool_id = item.get("id").and_then(Value::as_str).unwrap_or_default();
            let Some(position) = tool_positions.get(tool_id).copied() else {
                return;
            };
            let Some(ContentBlock::ToolUse(tool)) = blocks.get_mut(position) else {
                return;
            };
            match item_type {
                "command_execution" => {
                    tool.output = item
                        .get("output")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    tool.is_error = item.get("exit_code").and_then(Value::as_i64).unwrap_or(0) != 0;
                }
                "mcp_tool_call" => {
                    if let Some(result) = item.get("result") {
                        tool.output = normalize_mcp_result(result);
                    }
                    if let Some(error) = item.get("error") {
                        tool.output = normalize_error_value(error);
                        tool.is_error = true;
                    } else if item.get("status").and_then(Value::as_str) == Some("failed") {
                        tool.is_error = true;
                    }
                }
                "file_change" => {
                    tool.output = item
                        .get("patch")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("content").and_then(Value::as_str))
                        .unwrap_or_default()
                        .to_string();
                }
                _ => {}
            }
        }
        _ => {}
    }
}

// Convert codex error events into a visible text block.
fn handle_codex_error(value: &Value, blocks: &mut Vec<ContentBlock>) {
    let message = value
        .get("message")
        .map(normalize_error_value)
        .or_else(|| value.get("error").map(normalize_error_value))
        .unwrap_or_default();
    if !message.is_empty() {
        blocks.push(ContentBlock::Text { text: message });
    }
}

// Parse Claude JSON output into ordered content and tool-use blocks.
fn parse_claude_blocks(stdout: &str) -> Vec<ContentBlock> {
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return Vec::new();
    };
    let mut blocks = Vec::new();
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        for item in content {
            if let Some(block) = parse_claude_block(item) {
                blocks.push(block);
            }
        }
        pair_claude_tool_results(content, &mut blocks);
        if !blocks.is_empty() {
            return blocks;
        }
    }
    if let Some(text) = value.get("result").and_then(Value::as_str) {
        if !text.is_empty() {
            blocks.push(ContentBlock::Text {
                text: text.to_string(),
            });
        }
    }
    blocks
}

// Pair Claude tool_result blocks back into the matching tool-use blocks.
fn pair_claude_tool_results(content: &[Value], blocks: &mut [ContentBlock]) {
    let tool_positions: BTreeMap<String, usize> = blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| match block {
            ContentBlock::ToolUse(tool) => Some((tool.id.clone(), index)),
            _ => None,
        })
        .collect();
    for item in content {
        if item.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }
        let Some(tool_id) = item.get("tool_use_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(position) = tool_positions.get(tool_id).copied() else {
            continue;
        };
        let Some(ContentBlock::ToolUse(tool)) = blocks.get_mut(position) else {
            continue;
        };
        tool.output = normalize_claude_tool_result(item.get("content"));
        tool.is_error = item
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }
}

// Parse one Claude content block into the shared content model.
fn parse_claude_block(value: &Value) -> Option<ContentBlock> {
    let block_type = value.get("type").and_then(Value::as_str)?;
    match block_type {
        "text" => Some(ContentBlock::Text {
            text: value.get("text").and_then(Value::as_str)?.to_string(),
        }),
        "thinking" => Some(ContentBlock::Thinking {
            text: value
                .get("thinking")
                .or_else(|| value.get("text"))
                .and_then(Value::as_str)?
                .to_string(),
        }),
        "tool_use" => Some(ContentBlock::ToolUse(ToolCall {
            id: value
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            name: value
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            arguments: value
                .get("input")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
            output: String::new(),
            is_error: false,
        })),
        _ => None,
    }
}

// Convert a Claude tool-result payload into plain text.
fn normalize_claude_tool_result(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.to_string(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) => normalize_output_value(other),
        None => String::new(),
    }
}

// Parse opencode JSONL events into ordered content and tool-use blocks.
fn parse_opencode_blocks(stdout: &[u8]) -> Vec<ContentBlock> {
    let mut blocks = Vec::new();
    let mut tool_positions = BTreeMap::new();
    for line in String::from_utf8_lossy(stdout).lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match value.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = value
                    .get("part")
                    .and_then(Value::as_object)
                    .and_then(|part| part.get("text"))
                    .and_then(Value::as_str)
                {
                    if !text.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text: text.to_string(),
                        });
                    }
                }
            }
            Some("reasoning") => {
                if let Some(text) = extract_opencode_reasoning_text(&value) {
                    blocks.push(ContentBlock::Thinking { text });
                }
            }
            Some("tool_use") => handle_opencode_tool_use(&value, &mut blocks, &mut tool_positions),
            Some("error") => {
                let message = value
                    .get("error")
                    .map(normalize_error_value)
                    .unwrap_or_default();
                if !message.is_empty() {
                    blocks.push(ContentBlock::Text { text: message });
                }
            }
            _ => {}
        }
    }
    blocks
}

// Insert or update an opencode tool block based on a tool event.
fn handle_opencode_tool_use(
    value: &Value,
    blocks: &mut Vec<ContentBlock>,
    tool_positions: &mut BTreeMap<String, usize>,
) {
    let Some(part) = value.get("part").and_then(Value::as_object) else {
        return;
    };
    if part.get("type").and_then(Value::as_str) != Some("tool") {
        return;
    }
    let tool_id = part
        .get("callID")
        .or_else(|| part.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let state = part
        .get("state")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let tool_name = part
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("tool")
        .to_string();
    let position = if let Some(position) = tool_positions.get(&tool_id).copied() {
        position
    } else {
        let index = push_tool_block(
            blocks,
            ToolCall {
                id: tool_id.clone(),
                name: tool_name,
                arguments: state
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({})),
                output: String::new(),
                is_error: false,
            },
        );
        tool_positions.insert(tool_id.clone(), index);
        index
    };
    let Some(ContentBlock::ToolUse(tool)) = blocks.get_mut(position) else {
        return;
    };
    if let Some(output) = state.get("output") {
        tool.output = normalize_output_value(output);
    }
    if let Some(error) = state.get("error") {
        tool.output = normalize_error_value(error);
        tool.is_error = true;
    } else if state.get("status").and_then(Value::as_str) == Some("error") {
        tool.is_error = true;
    }
}

// Extract reasoning text from an opencode reasoning event.
fn extract_opencode_reasoning_text(value: &Value) -> Option<String> {
    let part = value.get("part").and_then(Value::as_object)?;
    if let Some(text) = part.get("text").and_then(Value::as_str) {
        if !text.is_empty() {
            return Some(text.to_string());
        }
    }
    let metadata = part.get("metadata").and_then(Value::as_object)?;
    let has_encrypted = metadata.values().any(|entry| {
        entry
            .as_object()
            .and_then(|entry| entry.get("reasoningEncryptedContent"))
            .is_some()
    });
    if has_encrypted {
        Some("[encrypted reasoning]".to_string())
    } else {
        None
    }
}

// Append a tool block and return its index in the block list.
fn push_tool_block(blocks: &mut Vec<ContentBlock>, tool: ToolCall) -> usize {
    blocks.push(ContentBlock::ToolUse(tool));
    blocks.len() - 1
}

// Convert an MCP tool result value into a readable text payload.
fn normalize_mcp_result(value: &Value) -> String {
    if let Some(content) = value.get("content").and_then(Value::as_array) {
        let texts: Vec<String> = content
            .iter()
            .filter_map(|entry| {
                if let Some(text) = entry.get("text").and_then(Value::as_str) {
                    Some(text.to_string())
                } else {
                    entry.as_str().map(ToString::to_string)
                }
            })
            .collect();
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    normalize_output_value(value)
}

// Convert any output-shaped JSON value into plain text.
fn normalize_output_value(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        text.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    }
}

// Convert any error-shaped JSON value into plain text.
fn normalize_error_value(value: &Value) -> String {
    if let Some(text) = value.get("message").and_then(Value::as_str) {
        text.to_string()
    } else if let Some(text) = value.as_str() {
        text.to_string()
    } else {
        serde_json::to_string(value).unwrap_or_default()
    }
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

    #[test]
    // Confirm codex event streams become structured text, thinking, and tool blocks.
    fn codex_blocks_are_parsed() {
        let input = br#"{"type":"item.started","item":{"id":"tool-1","type":"command_execution","command":"ls"}}
{"type":"item.completed","item":{"id":"tool-1","type":"command_execution","output":"file.txt","exit_code":0}}
{"type":"item.completed","item":{"type":"reasoning","text":"thinking"}}
{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#;
        let blocks = parse_codex_blocks(input);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::ToolUse(_)));
        assert!(matches!(blocks[1], ContentBlock::Thinking { .. }));
        assert!(matches!(blocks[2], ContentBlock::Text { .. }));
    }

    #[test]
    // Confirm Claude JSON content arrays become shared content blocks.
    fn claude_blocks_are_parsed() {
        let input = r#"{"content":[{"type":"thinking","thinking":"plan"},{"type":"text","text":"answer"},{"type":"tool_use","id":"1","name":"Read","input":{"path":"a"}}]}"#;
        let blocks = parse_claude_blocks(input);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(blocks[1], ContentBlock::Text { .. }));
        assert!(matches!(blocks[2], ContentBlock::ToolUse(_)));
    }

    #[test]
    // Confirm opencode event streams become shared text, thinking, and tool blocks.
    fn opencode_blocks_are_parsed() {
        let input = br#"{"type":"reasoning","part":{"text":"plan"}}
{"type":"tool_use","part":{"type":"tool","callID":"tool-1","tool":"read","state":{"input":{"path":"a"},"output":"hello","status":"completed"}}}
{"type":"text","part":{"text":"answer"}}"#;
        let blocks = parse_opencode_blocks(input);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::Thinking { .. }));
        assert!(matches!(blocks[1], ContentBlock::ToolUse(_)));
        assert!(matches!(blocks[2], ContentBlock::Text { .. }));
    }

    #[test]
    // Detect codex usage-limit text and return the default wait time when no duration is present.
    fn codex_limit_is_detected() {
        assert_eq!(
            detect_codex_usage_limit("Usage limit reached, try later"),
            Some(60)
        );
    }

    #[test]
    // Detect Claude usage-limit text and return the default wait time when no duration is present.
    fn claude_limit_is_detected() {
        assert_eq!(
            detect_claude_usage_limit("You are out of usage and your limit resets soon"),
            Some(60)
        );
    }

    #[test]
    // Detect opencode usage-limit text and parse retry seconds into minutes when present.
    fn opencode_limit_is_detected() {
        assert_eq!(
            detect_opencode_usage_limit("FreeUsageLimitError: retry after 180"),
            Some(4)
        );
    }

    #[test]
    // Parse the Claude fixture and keep the tool result attached to the matching tool block.
    fn claude_fixture_preserves_tool_output() {
        let input = include_str!("../tests/fixtures/claude_output.json");
        let blocks = parse_claude_blocks(input);
        assert_eq!(blocks.len(), 3);
        match &blocks[1] {
            ContentBlock::ToolUse(tool) => {
                assert_eq!(tool.name, "Read");
                assert_eq!(tool.output, "fn main() {}");
                assert!(!tool.is_error);
            }
            _ => panic!("expected tool block"),
        }
    }

    #[test]
    // Parse the opencode fixture and preserve reasoning, tool output, and final text order.
    fn opencode_fixture_preserves_block_order() {
        let input = include_bytes!("../tests/fixtures/opencode_output.jsonl");
        let blocks = parse_opencode_blocks(input);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(blocks[0], ContentBlock::Thinking { .. }));
        match &blocks[1] {
            ContentBlock::ToolUse(tool) => {
                assert_eq!(tool.name, "read");
                assert_eq!(tool.output, "pub fn lib() {}");
            }
            _ => panic!("expected tool block"),
        }
        assert!(matches!(blocks[2], ContentBlock::Text { .. }));
    }
}
