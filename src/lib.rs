//! Shared types for `acp-tap` (the wrapper) and `acp-tapd` (the dashboard).
//!
//! The wrapper stays deliberately dumb: it forwards bytes between an ACP client
//! and an ACP agent, and mirrors each complete line to the dashboard socket as a
//! [`WireFrame`]. All interpretation happens in the dashboard, so a parsing bug
//! can never break the agent it is watching.

use serde::{Deserialize, Serialize};

/// Which way a frame was travelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// Client to agent: requests and notifications from the editor/harness.
    ToAgent,
    /// Agent to client: responses and `session/update` notifications.
    ToClient,
}

/// One mirrored JSON-RPC line, as sent from the wrapper to the dashboard.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WireFrame {
    /// Human label for the tapped process (agent name, unit, or cwd basename).
    pub label: String,
    pub dir: Direction,
    /// Milliseconds since the Unix epoch.
    pub ts_ms: u64,
    /// The raw JSON-RPC line, unmodified.
    pub line: String,
}

/// A parsed, UI-facing event.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EventKind {
    SessionCreated {
        session_id: String,
    },
    TurnStarted {
        text: String,
    },
    Thought {
        text: String,
    },
    Message {
        text: String,
    },
    ToolCall {
        tool_id: String,
        title: String,
        tool_kind: String,
        status: String,
    },
    ToolUpdate {
        tool_id: String,
        status: String,
    },
    Plan {
        entries: usize,
    },
    TurnEnded {
        stop_reason: String,
    },
    Error {
        message: String,
    },
}

/// An event as broadcast to browsers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UiEvent {
    pub seq: u64,
    pub label: String,
    pub ts_ms: u64,
    pub session_id: Option<String>,
    #[serde(flatten)]
    pub event: EventKind,
}

/// Per-agent state shown in the sidebar.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AgentSnapshot {
    pub label: String,
    pub connected: bool,
    pub session_id: Option<String>,
    /// Set while a turn is in flight.
    pub turn_started_ms: Option<u64>,
    pub last_activity_ms: Option<u64>,
    pub turns: u64,
    pub tool_calls: u64,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Truncate to a character budget, appending an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Parse one mirrored line into zero or more UI events.
///
/// Unknown methods and malformed lines yield nothing rather than erroring: the
/// dashboard must survive protocol drift and partial frames.
pub fn parse_frame(frame: &WireFrame) -> Vec<(Option<String>, EventKind)> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&frame.line) else {
        return Vec::new();
    };

    let method = v.get("method").and_then(|m| m.as_str());
    let params = v.get("params");
    let session_id = params
        .and_then(|p| p.get("sessionId"))
        .and_then(|s| s.as_str())
        .map(str::to_string);

    // Responses carry no method; the ones worth surfacing are prompt results
    // (stopReason) and errors.
    if method.is_none() {
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return vec![(
                None,
                EventKind::Error {
                    message: msg.to_string(),
                },
            )];
        }
        if let Some(stop) = v
            .get("result")
            .and_then(|r| r.get("stopReason"))
            .and_then(|s| s.as_str())
        {
            return vec![(
                None,
                EventKind::TurnEnded {
                    stop_reason: stop.to_string(),
                },
            )];
        }
        if let Some(sid) = v
            .get("result")
            .and_then(|r| r.get("sessionId"))
            .and_then(|s| s.as_str())
        {
            return vec![(
                Some(sid.to_string()),
                EventKind::SessionCreated {
                    session_id: sid.to_string(),
                },
            )];
        }
        return Vec::new();
    }

    match method.unwrap() {
        "session/prompt" => {
            let text = params
                .and_then(|p| p.get("prompt"))
                .and_then(|p| p.as_array())
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            vec![(
                session_id,
                EventKind::TurnStarted {
                    text: truncate(&text, 2000),
                },
            )]
        }
        "session/update" => {
            let Some(update) = params.and_then(|p| p.get("update")) else {
                return Vec::new();
            };
            let kind = update
                .get("sessionUpdate")
                .and_then(|k| k.as_str())
                .unwrap_or("");
            let text_of = |field: &str| -> String {
                update
                    .get(field)
                    .and_then(|c| c.get("text"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let ev = match kind {
                "agent_thought_chunk" => EventKind::Thought {
                    text: text_of("content"),
                },
                "agent_message_chunk" => EventKind::Message {
                    text: text_of("content"),
                },
                "tool_call" => EventKind::ToolCall {
                    tool_id: update
                        .get("toolCallId")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    title: update
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    tool_kind: update
                        .get("kind")
                        .and_then(|t| t.as_str())
                        .unwrap_or("other")
                        .to_string(),
                    status: update
                        .get("status")
                        .and_then(|t| t.as_str())
                        .unwrap_or("pending")
                        .to_string(),
                },
                "tool_call_update" => EventKind::ToolUpdate {
                    tool_id: update
                        .get("toolCallId")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                    status: update
                        .get("status")
                        .and_then(|t| t.as_str())
                        .unwrap_or("")
                        .to_string(),
                },
                "plan" => EventKind::Plan {
                    entries: update
                        .get("entries")
                        .and_then(|e| e.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0),
                },
                _ => return Vec::new(),
            };
            vec![(session_id, ev)]
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(line: &str) -> WireFrame {
        WireFrame {
            label: "test".into(),
            dir: Direction::ToClient,
            ts_ms: 0,
            line: line.into(),
        }
    }

    #[test]
    fn ignores_malformed_lines() {
        assert!(parse_frame(&frame("not json")).is_empty());
        assert!(parse_frame(&frame("")).is_empty());
        assert!(parse_frame(&frame("{}")).is_empty());
    }

    #[test]
    fn parses_thought_and_message_chunks() {
        let l = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}}}"#;
        let out = parse_frame(&frame(l));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.as_deref(), Some("s1"));
        assert!(matches!(&out[0].1, EventKind::Thought { text } if text == "hmm"));
    }

    #[test]
    fn parses_tool_calls() {
        let l = r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s1","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"bash","kind":"execute","status":"pending"}}}"#;
        let out = parse_frame(&frame(l));
        assert!(
            matches!(&out[0].1, EventKind::ToolCall { tool_id, title, .. } if tool_id == "t1" && title == "bash")
        );
    }

    #[test]
    fn parses_turn_end_and_errors() {
        let out = parse_frame(&frame(
            r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#,
        ));
        assert!(
            matches!(&out[0].1, EventKind::TurnEnded { stop_reason } if stop_reason == "end_turn")
        );

        let out = parse_frame(&frame(
            r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32601,"message":"Method not found"}}"#,
        ));
        assert!(matches!(&out[0].1, EventKind::Error { message } if message == "Method not found"));
    }

    #[test]
    fn extracts_prompt_text() {
        let l = r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":"s1","prompt":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]}}"#;
        let out = parse_frame(&frame(l));
        assert!(matches!(&out[0].1, EventKind::TurnStarted { text } if text == "hello world"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        assert_eq!(truncate("héllo", 3), "hél…");
        assert_eq!(truncate("hi", 10), "hi");
    }
}
