use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Strongest,
    Fast,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct MCPServer {
    pub name: String,
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct UsageStats {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub cost_usd: f64,
}

impl UsageStats {
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl std::ops::AddAssign<&UsageStats> for UsageStats {
    fn add_assign(&mut self, rhs: &UsageStats) {
        self.input_tokens += rhs.input_tokens;
        self.output_tokens += rhs.output_tokens;
        self.cache_read_tokens += rhs.cache_read_tokens;
        self.cache_write_tokens += rhs.cache_write_tokens;
        self.cost_usd += rhs.cost_usd;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
    #[serde(default)]
    pub output: String,
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text { text: String },
    Thinking { text: String },
    ToolUse(ToolCall),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Turn {
    pub input: String,
    #[serde(default)]
    pub output: Vec<ContentBlock>,
    #[serde(default)]
    pub usage: UsageStats,
    #[serde(default)]
    pub duration_ms: u128,
}

impl Turn {
    pub fn result(&self) -> String {
        self.output
            .iter()
            .rev()
            .find_map(|block| match block {
                ContentBlock::Text { text } if !text.is_empty() => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Trajectory {
    pub agent: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub usage_limited: bool,
    #[serde(default)]
    pub system_prompt: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub mcp_servers: Vec<MCPServer>,
    #[serde(default)]
    pub turns: Vec<Turn>,
    #[serde(default)]
    pub usage: UsageStats,
    #[serde(default)]
    pub duration_ms: u128,
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

impl Trajectory {
    pub fn new(agent: impl Into<String>, model: impl Into<String>, session_id: impl Into<String>) -> Self {
        Self {
            agent: agent.into(),
            model: model.into(),
            session_id: session_id.into(),
            created_at: Some(Utc::now()),
            completed_at: None,
            usage_limited: false,
            system_prompt: String::new(),
            reasoning: String::new(),
            mcp_servers: Vec::new(),
            turns: Vec::new(),
            usage: UsageStats::default(),
            duration_ms: 0,
            metadata: BTreeMap::new(),
        }
    }

    pub fn result(&self) -> String {
        self.turns.last().map(Turn::result).unwrap_or_default()
    }

    pub fn total_tool_calls(&self) -> usize {
        self.turns
            .iter()
            .flat_map(|turn| turn.output.iter())
            .filter(|block| matches!(block, ContentBlock::ToolUse(_)))
            .count()
    }

    pub fn append_turn(&mut self, turn: Turn) {
        self.duration_ms += turn.duration_ms;
        self.usage += &turn.usage;
        self.turns.push(turn);
    }

    pub fn finalize(&mut self) {
        self.completed_at = Some(Utc::now());
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthSignal {
    pub present: bool,
    pub detail: String,
    #[serde(default)]
    pub credentials_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderHealth {
    pub provider: String,
    pub installed: bool,
    #[serde(default)]
    pub binary_path: Option<String>,
    #[serde(default)]
    pub auth: Option<AuthSignal>,
}
