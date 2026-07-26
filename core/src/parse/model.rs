//! Provider-neutral, read-time representation of a captured API call.

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    /// Anthropic Messages dialect (Claude Code and anything else speaking it).
    ClaudeCode,
    /// OpenAI dialect — Chat Completions or Responses. Codex and opencode.
    OpenAi,
    Unknown,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApproxSize {
    pub chars: usize,
    pub bytes: usize,
}

impl ApproxSize {
    pub fn of_text(text: &str) -> Self {
        Self {
            chars: text.chars().count(),
            bytes: text.len(),
        }
    }

    pub fn of_value(value: &Value) -> Self {
        Self::of_text(&value.to_string())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Timestamps {
    pub start: String,
    pub first_chunk: Option<String>,
    pub end: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SystemSegment {
    pub text: String,
    pub approx_size: ApproxSize,
    pub cache_control: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolDecl {
    pub name: String,
    pub description: Option<String>,
    pub input_schema: Option<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Tool,
    /// An inline system or developer turn. Both dialects send these; without a
    /// variant they collapse into `Other` and disappear from every count.
    System,
    Other,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    History,
    New,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Thinking,
    ToolUse,
    ToolResult,
    Image,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentTag {
    SystemReminder,
    LocalCommandCaveat,
    CommandName,
    CommandMessage,
    CommandArgs,
    LocalCommandStdout,
}

#[derive(Debug, Clone, Serialize)]
pub struct Block {
    pub kind: BlockKind,
    /// Explicit wrapper tag from a user-content text block, when present.
    /// This keeps injected metadata distinct from the user’s direct message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_tag: Option<ContentTag>,
    pub approx_size: ApproxSize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Turn {
    pub role: Role,
    pub origin: Origin,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Usage {
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cache_creation: Option<u64>,
    pub cache_read: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IntraSignals {
    pub block_counts: BlockCounts,
    pub oversized_blocks: Vec<BlockReference>,
    pub tool_calls: Vec<ToolCall>,
    pub undeclared_tool_calls: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct BlockCounts {
    pub text: usize,
    pub thinking: usize,
    pub tool_use: usize,
    pub tool_result: usize,
    pub image: usize,
    pub other: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockReference {
    pub turn: usize,
    pub block: usize,
    pub approx_size: ApproxSize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCall {
    pub name: String,
    pub input: Option<Value>,
    pub tool_use_id: Option<String>,
    pub result: Option<Block>,
}

#[derive(Debug, Clone, Serialize)]
pub struct NormalizedCall {
    pub id: i64,
    pub provider: Provider,
    pub model: Option<String>,
    pub session_key: Option<String>,
    pub timestamps: Timestamps,
    pub system: Vec<SystemSegment>,
    pub declared_tools: Vec<ToolDecl>,
    pub thread: Vec<Turn>,
    pub usage: Usage,
    pub intra: IntraSignals,
    pub response_status: Option<i64>,
    pub error: Option<String>,
    /// True for providers that do not have a semantic parser; raw data remains
    /// available through the existing call-detail endpoint.
    pub raw_fallback: bool,
}
