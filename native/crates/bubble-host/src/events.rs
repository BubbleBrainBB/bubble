//! What the host sends about a session: the agent's events, and the
//! questions only the person or the app can answer.

use serde::Deserialize;
use serde_json::Value;

/// One event of a turn (the TypeScript core's `AgentEvent`).
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A model request starts.
    TurnStart,
    TextDelta {
        content: String,
    },
    ReasoningDelta {
        content: String,
    },
    /// The model began writing a tool call.
    ToolCallStart {
        id: String,
        name: String,
    },
    /// The model finished writing a tool call; `arguments` is its JSON.
    ToolCallEnd {
        id: String,
        name: String,
        arguments: String,
    },
    /// The tool runs now.
    ToolStart {
        id: String,
        name: String,
        #[serde(default)]
        args: Value,
    },
    /// Progress of a running tool: a subagent's.
    ToolUpdate {
        id: String,
        name: String,
        update: SubagentUpdate,
    },
    ToolEnd {
        id: String,
        name: String,
        result: ToolResult,
    },
    /// A model request and its tools are done; another follows if the model
    /// called tools.
    #[serde(rename_all = "camelCase")]
    TurnEnd {
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(default)]
        will_continue: Option<bool>,
    },
    #[serde(rename_all = "camelCase")]
    ContextCompaction {
        status: String,
        #[serde(default)]
        pre_tokens: Option<u64>,
        #[serde(default)]
        post_tokens: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    ContextUsage {
        used_tokens: u64,
        #[serde(default)]
        context_window: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    ProviderRetry {
        attempt: u32,
        max_attempts: u32,
        reason: String,
    },
    /// What the person added mid-turn reached the model.
    InputApplied {
        id: String,
        content: String,
    },
    /// What the person added mid-turn starts a turn of its own after this one.
    InputQueued {
        id: String,
        content: String,
    },
    InputRejected {
        id: String,
        content: String,
        reason: String,
    },
    ModeChanged {
        mode: String,
    },
    AgentEnd,
    /// An event this client has no use for.
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResult {
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    /// "success", "blocked", "cancelled", "timeout", "command_error", ...
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
}

/// A subagent's progress, reported through the tool call that started or
/// waits on it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentUpdate {
    #[serde(default)]
    pub parent_tool_call_id: String,
    #[serde(default)]
    pub run_id: String,
    pub sub_agent_id: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    /// "queued", "running", "completed", "failed", "blocked", "cancelled".
    pub status: String,
    /// What the subagent itself is doing.
    #[serde(default)]
    pub child_event: Option<Box<Event>>,
    #[serde(default)]
    pub summary_delta: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
}

/// How a turn ended.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Terminal {
    Completed,
    Failed { message: String },
    Cancelled { message: String },
}

/// What a tool wants a yes for.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovalRequest {
    #[serde(rename_all = "camelCase")]
    Bash {
        command: String,
        #[serde(default)]
        cwd: String,
        #[serde(default)]
        background: bool,
        /// What "for the rest of the session" would cover; absent when only
        /// this one call can be allowed.
        #[serde(default)]
        session_grant: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Edit {
        path: String,
        #[serde(default)]
        diff: String,
        #[serde(default)]
        outside_workspace: bool,
        #[serde(default)]
        protected_path: bool,
    },
    #[serde(rename_all = "camelCase")]
    Write {
        path: String,
        #[serde(default)]
        diff: Option<String>,
        #[serde(default)]
        file_exists: bool,
        #[serde(default)]
        outside_workspace: bool,
        #[serde(default)]
        protected_path: bool,
    },
    Patch {
        path: String,
        #[serde(default)]
        diff: String,
    },
    Lsp {
        path: String,
        operation: String,
    },
    #[serde(rename_all = "camelCase")]
    AgentProfile {
        name: String,
        path: String,
        #[serde(default)]
        prompt_preview: String,
    },
    /// A tool Bubble does not implement itself: an MCP server's (`kind` is
    /// "mcp" and `title` the `mcp__server__tool` name).
    #[serde(rename_all = "camelCase")]
    ExternalTool {
        title: String,
        #[serde(default)]
        kind: String,
        #[serde(default)]
        raw_input: Value,
        #[serde(default)]
        session_grant: Option<String>,
    },
}

impl ApprovalRequest {
    /// Whether the yes can be remembered for the rest of the session.
    pub fn rememberable(&self) -> bool {
        match self {
            ApprovalRequest::Bash { session_grant, .. } | ApprovalRequest::ExternalTool { session_grant, .. } => {
                session_grant.is_some()
            }
            _ => false,
        }
    }
}

/// The person's answer to an [`ApprovalRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Approval {
    Once,
    /// Also for what the request's session grant covers, until the session ends.
    ForSession,
    Deny(Option<String>),
}

impl Approval {
    pub(crate) fn to_json(&self) -> Value {
        match self {
            Approval::Once => serde_json::json!({ "action": "approve" }),
            Approval::ForSession => serde_json::json!({ "action": "approve", "remember": "session" }),
            Approval::Deny(feedback) => serde_json::json!({ "action": "reject", "feedback": feedback }),
        }
    }
}

/// The turn's configuration once the host resolved it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStarted {
    #[serde(default)]
    pub provider_id: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub thinking_level: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub tools: Vec<String>,
}

/// What reaches the subscriber of a session.
#[derive(Debug)]
pub enum SessionMessage {
    Started(TurnStarted),
    Event {
        turn_id: String,
        event: Event,
        /// Set on the last event of a turn.
        terminal: Option<Terminal>,
    },
    /// Answer with [`crate::Host::approve`].
    Approval {
        id: String,
        tool_call_id: Option<String>,
        request: ApprovalRequest,
    },
    /// The agent asks the person something; answer with [`crate::Host::answer`].
    Question {
        id: String,
        request: Value,
    },
    /// The agent proposes a plan; answer with [`crate::Host::answer`].
    Plan {
        id: String,
        plan: String,
    },
    /// The folder's own settings want trust; answer with [`crate::Host::answer`].
    Trust {
        id: String,
        cwd: String,
        pending: Value,
    },
    /// A tool the app runs; answer with [`crate::Host::tool_result`].
    ToolCall {
        id: String,
        tool_call_id: Option<String>,
        name: String,
        args: Value,
    },
    /// The host no longer waits for the answer to request `id`.
    Withdrawn {
        id: String,
    },
    /// The host process ended; nothing more comes.
    Lost(String),
}
