//! Server-side drop list for tool events (`[ingest]`, #1241).
//!
//! An operator can name tool calls the server should never store, whatever the
//! client sends: tool-use ids by prefix (Claude Code marks plugin tool calls
//! with their own id prefix) and tool names exactly. A matching event is
//! acknowledged like any other policy drop, before it reaches the sanitizer,
//! the rate limiter, the ingest semaphore or storage. The lists are read once
//! at startup and bounded, so the check stays cheap on the hook path.

use crate::payload::{HookEnvelope, HookEvent};

/// Most entries either list may hold.
pub const MAX_DROP_ENTRIES: usize = 64;
/// Longest entry, in characters.
pub const MAX_DROP_ENTRY_CHARS: usize = 128;

/// Tool events the server drops by tool-use id prefix or exact tool name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestDropList {
    tool_use_id_prefixes: Vec<String>,
    tool_names: Vec<String>,
}

impl IngestDropList {
    /// Validate and build the list.
    ///
    /// # Errors
    /// Returns a message naming the offending list when either holds more than
    /// [`MAX_DROP_ENTRIES`] entries, or an entry that is blank, longer than
    /// [`MAX_DROP_ENTRY_CHARS`] characters, or contains a control character.
    pub fn new(tool_use_id_prefixes: Vec<String>, tool_names: Vec<String>) -> Result<Self, String> {
        validate("drop_tool_use_id_prefixes", &tool_use_id_prefixes)?;
        validate("drop_tool_names", &tool_names)?;
        Ok(Self {
            tool_use_id_prefixes,
            tool_names,
        })
    }

    /// Whether this tool event is on the list. Only tool events can match.
    #[must_use]
    pub fn matches(&self, env: &HookEnvelope) -> bool {
        if self.tool_use_id_prefixes.is_empty() && self.tool_names.is_empty() {
            return false;
        }
        if !matches!(env.event, HookEvent::PreToolUse | HookEvent::PostToolUse) {
            return false;
        }
        let tool_use_id = crate::payload::event_tool_use_id(&env.raw);
        if let Some(id) = tool_use_id.as_deref()
            && self
                .tool_use_id_prefixes
                .iter()
                .any(|prefix| id.starts_with(prefix.as_str()))
        {
            return true;
        }
        crate::payload::event_tool_name(&env.raw)
            .is_some_and(|name| self.tool_names.contains(&name))
    }
}

fn validate(list: &str, entries: &[String]) -> Result<(), String> {
    if entries.len() > MAX_DROP_ENTRIES {
        return Err(format!(
            "[ingest].{list} holds {} entries; the limit is {MAX_DROP_ENTRIES}",
            entries.len()
        ));
    }
    for entry in entries {
        if entry.trim().is_empty() {
            return Err(format!("[ingest].{list} has a blank entry"));
        }
        if entry.chars().count() > MAX_DROP_ENTRY_CHARS {
            return Err(format!(
                "[ingest].{list} has an entry longer than {MAX_DROP_ENTRY_CHARS} characters"
            ));
        }
        if entry.chars().any(char::is_control) {
            return Err(format!(
                "[ingest].{list} has an entry with a control character"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payload::HookQuery;

    fn event(name: &str, body: serde_json::Value) -> HookEnvelope {
        HookEnvelope::from_query_and_body(
            HookQuery {
                event: name.into(),
                agent: Some("claude-code".into()),
                ..Default::default()
            },
            body,
        )
    }

    fn list() -> IngestDropList {
        IngestDropList::new(
            vec!["toolu_plugin_".into()],
            vec!["mcp__noisy__poll".into()],
        )
        .unwrap()
    }

    #[test]
    fn a_tool_use_id_prefix_or_exact_tool_name_matches() {
        let by_id = event(
            "post-tool-use",
            serde_json::json!({ "session_id": "s", "tool_name": "Read", "tool_use_id": "toolu_plugin_42" }),
        );
        assert!(list().matches(&by_id));
        let by_name = event(
            "pre-tool-use",
            serde_json::json!({ "session_id": "s", "tool_name": "mcp__noisy__poll", "tool_use_id": "toolu_01" }),
        );
        assert!(list().matches(&by_name));
    }

    #[test]
    fn other_tools_prefixes_inside_ids_and_non_tool_events_do_not_match() {
        let ordinary = event(
            "post-tool-use",
            serde_json::json!({ "session_id": "s", "tool_name": "Read", "tool_use_id": "toolu_01abc" }),
        );
        assert!(!list().matches(&ordinary));
        // A prefix matches at the start only, and a name only exactly.
        let embedded = event(
            "post-tool-use",
            serde_json::json!({ "session_id": "s", "tool_name": "mcp__noisy__poll_more", "tool_use_id": "x_toolu_plugin_1" }),
        );
        assert!(!list().matches(&embedded));
        let prompt = event(
            "user-prompt-submit",
            serde_json::json!({ "session_id": "s", "tool_name": "mcp__noisy__poll", "prompt": "hi" }),
        );
        assert!(!list().matches(&prompt));
        assert!(!IngestDropList::default().matches(&ordinary));
    }

    #[test]
    fn entries_are_bounded_and_must_be_printable() {
        assert!(IngestDropList::new(vec![" ".into()], Vec::new()).is_err());
        assert!(IngestDropList::new(Vec::new(), vec!["a\nb".into()]).is_err());
        assert!(
            IngestDropList::new(vec!["x".repeat(MAX_DROP_ENTRY_CHARS + 1)], Vec::new()).is_err()
        );
        let too_many = (0..=MAX_DROP_ENTRIES).map(|i| format!("t{i}")).collect();
        assert!(IngestDropList::new(Vec::new(), too_many).is_err());
        assert!(IngestDropList::new(vec!["x".repeat(MAX_DROP_ENTRY_CHARS)], Vec::new()).is_ok());
    }
}
