use serde::{Deserialize, Serialize};

/// Chat completion request sent to the backend
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ChatContext>,
    pub stream: bool,
}

/// A single message in the chat conversation
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

/// Role of the message sender
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

/// Context for the chat request (project scope, mode)
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatContext {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ChatMode>,
}

/// Chat mode determines which tools and system prompt the backend uses
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatMode {
    Rag,
    Copilot,
}

/// Server-Sent Event from the chat completion stream
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatEvent {
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolCallStart {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "toolName")]
        tool_name: String,
    },
    ToolCallDelta {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        #[serde(rename = "argsPartial")]
        args_partial: String,
    },
    ToolCallResult {
        #[serde(rename = "toolCallId")]
        tool_call_id: String,
        result: serde_json::Value,
    },
    Done,
}

/// Tracks a tool call being assembled from streaming events
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallInfo {
    pub id: String,
    pub name: String,
    pub args_buffer: String,
    pub result: Option<serde_json::Value>,
}

// ─── Session types (mirrors backend ChatRoutes / models.scala) ───────────────

/// Response from GET /chat/sessions
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatSessionResponse {
    pub id: String,
    pub title: Option<String>,
    pub context: Option<ChatContextResponse>,
    pub created_at: String,
    pub updated_at: String,
}

/// Context in session response
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatContextResponse {
    pub project_id: Option<String>,
    pub sop_id: Option<String>,
    pub mode: Option<String>,
}

/// Response from GET /chat/sessions/{id}/messages
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessageResponse {
    pub id: String,
    pub session_id: String,
    pub role: ChatRole,
    pub content: String,
    pub tool_calls: Option<serde_json::Value>,
    pub created_at: String,
}

/// Request body for POST /chat/sessions
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSessionRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ChatContext>,
}

/// Request body for POST /chat/sessions/{id}/messages
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveMessageRequest {
    pub role: ChatRole,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<serde_json::Value>,
}

/// Request body for PATCH /chat/sessions/{id}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateSessionTitleRequest {
    pub title: String,
}

// ─── Streaming UI update channel ─────────────────────────────────────────────

/// Messages sent from the SSE producer task to the UI consumer.
/// Each variant triggers exactly one signal update → one render cycle.
#[derive(Debug)]
pub enum StreamingUpdate {
    /// Accumulated text so far (full, not delta)
    Text(String),
    /// A new tool call started
    ToolStart(ToolCallInfo),
    /// Tool call arguments appended
    ToolDelta { id: String, args: String },
    /// Tool call result received
    ToolResult {
        id: String,
        result: serde_json::Value,
    },
    /// Stream finished — final assistant text to persist
    AssistantDone(String),
    /// Stream error
    Error(String),
}
