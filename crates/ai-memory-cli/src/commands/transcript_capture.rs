//! `[capture] ignore_paths` for imported native transcripts.
//!
//! `ai-memory backfill` and the `ai-memory run` ledger read a harness's own
//! transcript instead of receiving hook events, so the live hook's capture
//! policy never saw those tool calls. This applies the same decision table
//! rather than a second matcher: each recorded tool call is rebuilt in its
//! agent's hook shape (`tool_call_payload`) and inspected by the checkout's
//! `CapturePolicy`, resolved from the working directory its own record names
//! when the adapter keeps one; a call the live hook would reroute to another
//! checkout also answers to that checkout's admission gate, server route and
//! policy; and each tool result inherits the decision of the call it answers.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ai_memory_core::{AgentKind, NewWorkstreamEvent, WorkstreamEventKind};
use ai_memory_hooks::{
    CaptureDecision, CaptureDisposition, CapturePolicy, PolicyState, ToolFamily, tool_call_payload,
};
use ai_memory_workstream::RECORD_CWD_KEY;
use serde_json::Value;

use super::hook::{
    checkout_admits_capture, delivers_to_same_server, lexical_capture_cwd,
    tool_location_routing_cwd,
};
use super::hook_capture::capture_policy;

/// What an import does with one transcript event.
#[derive(Clone, Debug)]
pub(crate) enum EventCapture {
    /// Import the event as recorded.
    Keep,
    /// Import nothing for this event.
    Drop,
    /// Record that the tool ran, never its arguments or output.
    MetadataOnly(CaptureDecision),
}

impl EventCapture {
    const fn rank(&self) -> u8 {
        match self {
            Self::Keep => 0,
            Self::MetadataOnly(_) => 1,
            Self::Drop => 2,
        }
    }

    fn stricter(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }

    fn from_decision(decision: CaptureDecision) -> Self {
        match decision.protocol().disposition() {
            CaptureDisposition::Keep => Self::Keep,
            CaptureDisposition::Drop => Self::Drop,
            CaptureDisposition::MetadataOnly => Self::MetadataOnly(decision),
        }
    }
}

/// The capture policy of the checkout a transcript was recorded in.
pub(crate) struct TranscriptCapture {
    data_dir: PathBuf,
    admitted: bool,
    policy: CapturePolicy,
    cwd: String,
    inspection_cwd: String,
}

impl TranscriptCapture {
    /// Resolve the live hook's admission gate (this install's capture mode
    /// under `data_dir`) and the nearest marker's policy for `cwd`, exactly as
    /// the hook does for an event reporting that cwd.
    pub(crate) fn for_checkout(data_dir: &Path, cwd: &Path) -> Self {
        let cwd = cwd.to_string_lossy().into_owned();
        Self {
            data_dir: data_dir.to_path_buf(),
            admitted: checkout_admits_capture(data_dir, &cwd),
            policy: capture_policy(&cwd),
            inspection_cwd: lexical_capture_cwd(&cwd),
            cwd,
        }
    }

    /// Whether the live hook would emit anything at all from this checkout.
    /// Under allowlist capture mode an unmarked checkout admits nothing.
    pub(crate) const fn admits_capture(&self) -> bool {
        self.admitted
    }

    /// One decision per event, in transcript order. A checkout the hook's
    /// admission gate refuses drops every event, prompts included, as the
    /// live hook emits none. Otherwise messages are kept, as live prompt
    /// capture keeps them, except the output of a Claude Code `!` shell
    /// command, which live capture never receives.
    ///
    /// An event whose record names its own working directory is judged from
    /// there, as the live hook judges an event by its own `cwd`: that
    /// directory must pass the same admission gate and deliver to the same
    /// server as this checkout, or the event is dropped.
    ///
    /// A result answers the call with the same native id. Some adapters record
    /// no id, so a result without a matching call takes the strictest decision
    /// of every call since the last user message: parallel calls may answer
    /// out of order, and a positional guess could hand an excluded read's
    /// output the decision of a public one. A result with no call at all is
    /// dropped under a policy, since nothing proves it missed every ignored
    /// path.
    pub(crate) fn decide(&self, events: &[NewWorkstreamEvent]) -> Vec<EventCapture> {
        if !self.admitted {
            return vec![EventCapture::Drop; events.len()];
        }
        let mut elsewhere = HashMap::new();
        let mut by_id: HashMap<&str, EventCapture> = HashMap::new();
        let mut turn: Vec<EventCapture> = Vec::new();
        let mut out = Vec::with_capacity(events.len());
        for event in events {
            let cwd = record_cwd(event).unwrap_or(&self.cwd);
            let admitted = cwd == self.cwd || self.admits_elsewhere(cwd, &mut elsewhere);
            let capture = match event.kind {
                WorkstreamEventKind::ToolCall => {
                    let capture = if admitted {
                        self.decide_call(event, cwd, &mut elsewhere)
                    } else {
                        EventCapture::Drop
                    };
                    if let Some(id) = call_id(event) {
                        by_id.insert(id, capture.clone());
                    }
                    turn.push(capture.clone());
                    capture
                }
                WorkstreamEventKind::ToolResult if !admitted => EventCapture::Drop,
                WorkstreamEventKind::ToolResult => {
                    match call_id(event).and_then(|id| by_id.get(id)) {
                        Some(capture) => capture.clone(),
                        None => turn
                            .iter()
                            .cloned()
                            .reduce(EventCapture::stricter)
                            .unwrap_or_else(|| self.orphan_result()),
                    }
                }
                WorkstreamEventKind::Message => {
                    if matches!(event.role.as_deref(), Some("user" | "human")) {
                        turn.clear();
                    }
                    if admitted && !shell_mode_output(event) {
                        EventCapture::Keep
                    } else {
                        EventCapture::Drop
                    }
                }
                WorkstreamEventKind::Compaction
                | WorkstreamEventKind::Checkpoint
                | WorkstreamEventKind::Annotation => EventCapture::Keep,
            };
            out.push(capture);
        }
        out
    }

    /// Keep the import's events the policy keeps, and strip the ones it makes
    /// metadata-only, so excluded tool content never enters the run ledger.
    pub(crate) fn filter_ledger_events(
        &self,
        events: Vec<NewWorkstreamEvent>,
    ) -> Vec<NewWorkstreamEvent> {
        let decisions = self.decide(&events);
        events
            .into_iter()
            .zip(decisions)
            .filter_map(|(event, capture)| match capture {
                EventCapture::Keep => Some(event),
                EventCapture::Drop => None,
                EventCapture::MetadataOnly(decision) => Some(withheld(event, &decision)),
            })
            .map(|mut event| {
                // The record's cwd only steers the decision above.
                if let Some(metadata) = event.metadata.as_object_mut() {
                    metadata.remove(RECORD_CWD_KEY);
                }
                event
            })
            .collect()
    }

    fn orphan_result(&self) -> EventCapture {
        if self.policy.state() == PolicyState::Inactive {
            EventCapture::Keep
        } else {
            EventCapture::Drop
        }
    }

    /// Whether the live hook would deliver an event reported from `cwd`, a
    /// directory other than this checkout's, to this import's server at all:
    /// the hook's admission gate there (capture mode, marker opt-in, readable
    /// routing marker) and the same server-profile route.
    fn admits_elsewhere(&self, cwd: &str, seen: &mut HashMap<String, bool>) -> bool {
        *seen.entry(cwd.to_owned()).or_insert_with(|| {
            checkout_admits_capture(&self.data_dir, cwd)
                && delivers_to_same_server(&self.data_dir, &self.cwd, cwd)
        })
    }

    /// Judge one call as the live hook would for an event reporting `cwd`,
    /// under this checkout's policy too, since the import stays in this scope.
    fn decide_call(
        &self,
        event: &NewWorkstreamEvent,
        cwd: &str,
        elsewhere: &mut HashMap<String, bool>,
    ) -> EventCapture {
        let (name, args) = recorded_call(event);
        let unreadable = args.is_none();
        let raw = tool_call_payload(event.agent, name, args.as_ref(), call_id(event));
        let inspect = |policy: &CapturePolicy, cwd: &str| {
            inspect_call(
                policy,
                event.agent,
                &raw,
                &lexical_capture_cwd(cwd),
                unreadable,
            )
        };
        let mut capture = if cwd == self.cwd {
            inspect_call(
                &self.policy,
                event.agent,
                &raw,
                &self.inspection_cwd,
                unreadable,
            )
        } else {
            inspect(&self.policy, cwd).stricter(inspect(&capture_policy(cwd), cwd))
        };
        // The live hook would deliver this call to that checkout under its
        // admission gate, route and policy. An import stays in this scope, so
        // both policies apply, and a destination the hook would refuse or send
        // to another server drops the call.
        if let Some(destination) = tool_location_routing_cwd(event.agent, &raw, cwd) {
            if !self.admits_elsewhere(&destination, elsewhere) {
                return EventCapture::Drop;
            }
            capture = capture.stricter(inspect(&capture_policy(&destination), &destination));
        }
        capture
    }
}

/// One policy's decision for a rebuilt call. A shell command cut by the
/// adapter's size cap, or recorded as unparseable text, cannot be matched, and
/// its output may name any path.
fn inspect_call(
    policy: &CapturePolicy,
    agent: AgentKind,
    raw: &Value,
    inspection_cwd: &str,
    unreadable: bool,
) -> EventCapture {
    let decision = policy.inspect(agent, raw, inspection_cwd);
    if unreadable
        && decision.protocol().tool_family() == ToolFamily::NonFile
        && policy.state() != PolicyState::Inactive
    {
        return EventCapture::Drop;
    }
    EventCapture::from_decision(decision)
}

/// The absolute working directory the event's own record names, if any.
fn record_cwd(event: &NewWorkstreamEvent) -> Option<&str> {
    event
        .metadata
        .get(RECORD_CWD_KEY)
        .and_then(Value::as_str)
        .filter(|cwd| Path::new(cwd).is_absolute())
}

/// Claude Code records a `!` shell command's output as user messages made
/// only of these blocks. Live capture never receives that output.
fn shell_mode_output(event: &NewWorkstreamEvent) -> bool {
    const TAGS: [&str; 4] = [
        "bash-stdout",
        "bash-stderr",
        "local-command-stdout",
        "local-command-stderr",
    ];
    if event.agent != AgentKind::ClaudeCode {
        return false;
    }
    let mut rest = event.content.trim_start();
    if rest.is_empty() {
        return false;
    }
    while !rest.is_empty() {
        let Some(tag) = TAGS.iter().find(|tag| {
            rest.strip_prefix('<')
                .and_then(|rest| rest.strip_prefix(**tag))
                .is_some_and(|rest| rest.starts_with('>'))
        }) else {
            return false;
        };
        let close = format!("</{tag}>");
        // A block cut by the adapter's size cap has no closing tag.
        rest = rest
            .find(&close)
            .map_or("", |end| &rest[end + close.len()..])
            .trim_start();
    }
    true
}

/// The native id linking a tool call and its result, when the adapter kept one.
fn call_id(event: &NewWorkstreamEvent) -> Option<&str> {
    ["tool_use_id", "tool_call_id"]
        .iter()
        .find_map(|key| event.metadata.get(*key).and_then(Value::as_str))
        .filter(|id| !id.is_empty())
}

/// The tool name and arguments of a recorded call. Adapters write a call as
/// `{name}: {arguments}`, the arguments usually JSON. `None` means they could
/// not be recovered: truncated, or recorded as plain text.
fn recorded_call(event: &NewWorkstreamEvent) -> (&str, Option<Value>) {
    let name = event
        .metadata
        .get("tool")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .or_else(|| event.content.split_once(": ").map(|(name, _)| name))
        .unwrap_or("");
    let args = event
        .content
        .strip_prefix(name)
        .and_then(|rest| rest.strip_prefix(':'))
        .map(str::trim)
        .and_then(|rest| {
            if rest.is_empty() {
                Some(Value::Null)
            } else {
                serde_json::from_str(rest).ok()
            }
        });
    (name, args)
}

/// A ledger event with its content replaced by the tool family, keeping only
/// the labels that pair it with the other half of the call.
fn withheld(mut event: NewWorkstreamEvent, decision: &CaptureDecision) -> NewWorkstreamEvent {
    let family = serde_json::to_value(decision.protocol().tool_family())
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());
    let half = if event.kind == WorkstreamEventKind::ToolCall {
        "call"
    } else {
        "output"
    };
    event.content = format!("[{family} tool {half} withheld by the capture policy]");
    let mut metadata = serde_json::Map::new();
    for key in ["tool", "tool_use_id", "tool_call_id", "parent_id"] {
        if let Some(value) = event.metadata.get(key).filter(|value| value.is_string()) {
            metadata.insert(key.into(), value.clone());
        }
    }
    event.metadata = Value::Object(metadata);
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn event(
        kind: WorkstreamEventKind,
        role: &str,
        content: &str,
        metadata: Value,
    ) -> NewWorkstreamEvent {
        NewWorkstreamEvent {
            event_id: "evt".into(),
            agent: AgentKind::ClaudeCode,
            native_session_id: "sid".into(),
            source_record_id: None,
            kind,
            role: Some(role.into()),
            content: content.into(),
            occurred_at: None,
            metadata,
        }
    }

    fn checkout(ignore: Option<&str>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        if let Some(pattern) = ignore {
            std::fs::write(
                dir.path().join(".ai-memory.toml"),
                format!("[capture]\nignore_paths = [\"{pattern}\"]\n"),
            )
            .unwrap();
        }
        dir
    }

    fn read_call(path: &Path, id: Option<&str>) -> NewWorkstreamEvent {
        let mut metadata = json!({"tool": "Read"});
        if let Some(id) = id {
            metadata["tool_use_id"] = id.into();
        }
        event(
            WorkstreamEventKind::ToolCall,
            "assistant",
            &format!("Read: {}", json!({"file_path": path})),
            metadata,
        )
    }

    fn result(content: &str, id: Option<&str>) -> NewWorkstreamEvent {
        let metadata = id.map_or_else(|| json!({}), |id| json!({"tool_use_id": id}));
        event(WorkstreamEventKind::ToolResult, "tool", content, metadata)
    }

    fn kinds(captures: &[EventCapture]) -> Vec<&'static str> {
        captures
            .iter()
            .map(|capture| match capture {
                EventCapture::Keep => "keep",
                EventCapture::Drop => "drop",
                EventCapture::MetadataOnly(_) => "metadata",
            })
            .collect()
    }

    /// Paired by id, an excluded read drops both halves while a public read in
    /// the same turn is kept — even when the results answer out of order.
    #[test]
    fn an_excluded_read_drops_its_call_and_result_by_id() {
        let repo = checkout(Some("docs/adr/**"));
        let capture = TranscriptCapture::for_checkout(repo.path(), repo.path());
        let events = [
            event(WorkstreamEventKind::Message, "user", "read both", json!({})),
            read_call(&repo.path().join("docs/adr/1.md"), Some("adr")),
            read_call(&repo.path().join("src/lib.rs"), Some("pub")),
            result("public code", Some("pub")),
            result("ADR secret", Some("adr")),
            event(WorkstreamEventKind::Message, "assistant", "done", json!({})),
        ];
        assert_eq!(
            kinds(&capture.decide(&events)),
            ["keep", "drop", "keep", "keep", "drop", "keep"]
        );
    }

    /// Without ids, a result takes the strictest decision of its turn, so a
    /// reordered pair cannot leak; the next user turn starts clean.
    #[test]
    fn id_less_results_take_the_strictest_call_of_their_turn() {
        let repo = checkout(Some("docs/adr/**"));
        let capture = TranscriptCapture::for_checkout(repo.path(), repo.path());
        let events = [
            event(WorkstreamEventKind::Message, "user", "turn one", json!({})),
            read_call(&repo.path().join("docs/adr/1.md"), None),
            read_call(&repo.path().join("src/lib.rs"), None),
            result("public code", None),
            result("ADR secret", None),
            event(WorkstreamEventKind::Message, "user", "turn two", json!({})),
            read_call(&repo.path().join("src/lib.rs"), None),
            result("public code again", None),
        ];
        assert_eq!(
            kinds(&capture.decide(&events)),
            [
                "keep", "drop", "keep", "drop", "drop", "keep", "keep", "keep"
            ]
        );
    }

    #[test]
    fn unrecoverable_arguments_and_orphan_results_fail_closed_under_a_policy() {
        let repo = checkout(Some("docs/adr/**"));
        let capture = TranscriptCapture::for_checkout(repo.path(), repo.path());
        let truncated_shell = event(
            WorkstreamEventKind::ToolCall,
            "assistant",
            "Bash: {\"command\":\"cat docs/adr/1.md && echo",
            json!({"tool": "Bash"}),
        );
        let truncated_read = event(
            WorkstreamEventKind::ToolCall,
            "assistant",
            "Read: {\"file_path\":\"docs/ad",
            json!({"tool": "Read"}),
        );
        assert_eq!(
            kinds(&capture.decide(&[
                truncated_shell,
                truncated_read,
                event(WorkstreamEventKind::Message, "user", "next", json!({})),
                result("orphan output", None),
            ])),
            ["drop", "metadata", "keep", "drop"]
        );
    }

    /// No marker: nothing changes, including id-less and orphan results.
    #[test]
    fn without_a_policy_every_event_is_kept() {
        let repo = checkout(None);
        let capture = TranscriptCapture::for_checkout(repo.path(), repo.path());
        let events = [
            read_call(&repo.path().join("docs/adr/1.md"), None),
            result("ADR text", None),
            result("orphan", Some("missing")),
        ];
        assert_eq!(kinds(&capture.decide(&events)), ["keep", "keep", "keep"]);
    }

    /// A read whose absolute target is another marked checkout answers to that
    /// checkout's `ignore_paths` too, as the live hook's reroute would.
    #[test]
    fn a_call_into_another_checkout_honors_its_policy() {
        let source = checkout(None);
        let other = checkout(Some("private/**"));
        let capture = TranscriptCapture::for_checkout(source.path(), source.path());
        let events = [
            read_call(&other.path().join("private/key.txt"), Some("a")),
            result("private", Some("a")),
            read_call(&other.path().join("open.txt"), Some("b")),
            result("open", Some("b")),
        ];
        assert_eq!(
            kinds(&capture.decide(&events)),
            ["drop", "drop", "keep", "keep"]
        );
    }

    /// Under allowlist capture mode the live hook emits nothing — prompts
    /// included — from a checkout without a marker; an import drops every
    /// event there. A marked checkout and the default denylist mode keep them.
    #[test]
    fn allowlist_mode_admits_nothing_from_an_unmarked_checkout() {
        let allowlist = tempfile::tempdir().unwrap();
        std::fs::write(
            allowlist.path().join(super::super::hook::CAPTURE_MODE_FILE),
            "allowlist\n",
        )
        .unwrap();
        let denylist = tempfile::tempdir().unwrap();
        let unmarked = checkout(None);
        let marked = checkout(Some("docs/adr/**"));
        let events = |repo: &Path| {
            vec![
                event(WorkstreamEventKind::Message, "user", "a prompt", json!({})),
                read_call(&repo.join("src/lib.rs"), Some("pub")),
                result("public code", Some("pub")),
                event(WorkstreamEventKind::Message, "assistant", "done", json!({})),
            ]
        };

        let refused = TranscriptCapture::for_checkout(allowlist.path(), unmarked.path());
        assert!(!refused.admits_capture());
        assert_eq!(
            kinds(&refused.decide(&events(unmarked.path()))),
            ["drop", "drop", "drop", "drop"]
        );
        assert!(
            refused
                .filter_ledger_events(events(unmarked.path()))
                .is_empty()
        );

        let opted_in = TranscriptCapture::for_checkout(allowlist.path(), marked.path());
        assert!(opted_in.admits_capture());
        assert_eq!(
            kinds(&opted_in.decide(&events(marked.path()))),
            ["keep", "keep", "keep", "keep"]
        );
        let default = TranscriptCapture::for_checkout(denylist.path(), unmarked.path());
        assert!(default.admits_capture());
        assert_eq!(
            kinds(&default.decide(&events(unmarked.path()))),
            ["keep", "keep", "keep", "keep"]
        );
    }

    fn at(mut event: NewWorkstreamEvent, cwd: &Path) -> NewWorkstreamEvent {
        event.metadata[RECORD_CWD_KEY] = cwd.to_string_lossy().into_owned().into();
        event
    }

    /// The live hook judges every event — prompts included — by its own
    /// `cwd`. A record written after the session moved into a repository
    /// the hook would not capture under allowlist mode is dropped; records in
    /// a subdirectory of the opted-in checkout are kept, and the record cwd
    /// never reaches the ledger.
    #[test]
    fn a_record_cwd_answers_to_the_hook_admission_gate_there() {
        let data_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            data_dir.path().join(super::super::hook::CAPTURE_MODE_FILE),
            "allowlist\n",
        )
        .unwrap();
        let marked = checkout(Some("docs/adr/**"));
        // Outside `$HOME` a marker lookup stops at the git root.
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(marked.path())
            .status()
            .unwrap();
        assert!(init.success());
        let subdir = marked.path().join("src");
        std::fs::create_dir_all(&subdir).unwrap();
        let unmarked = checkout(None);
        let capture = TranscriptCapture::for_checkout(data_dir.path(), marked.path());
        let events = |cwd: &Path| {
            vec![
                at(
                    event(WorkstreamEventKind::Message, "user", "a prompt", json!({})),
                    cwd,
                ),
                at(read_call(&cwd.join("lib.rs"), Some("r")), cwd),
                at(result("code", Some("r")), cwd),
            ]
        };
        assert_eq!(
            kinds(&capture.decide(&events(unmarked.path()))),
            ["drop", "drop", "drop"]
        );
        assert_eq!(
            kinds(&capture.decide(&events(&subdir))),
            ["keep", "keep", "keep"]
        );
        let uploaded = capture.filter_ledger_events(events(&subdir));
        assert!(
            uploaded
                .iter()
                .all(|event| event.metadata.get(RECORD_CWD_KEY).is_none()),
            "{uploaded:?}"
        );
    }

    /// A Claude Code `!` command's output is recorded as user messages made
    /// only of `<bash-stdout>`/`<bash-stderr>` (or `<local-command-stdout>`)
    /// blocks, which live capture never receives. The typed command and an
    /// ordinary prompt quoting such a tag are kept, as is another agent's
    /// message.
    #[test]
    fn claude_shell_mode_output_is_not_imported() {
        let repo = checkout(None);
        let capture = TranscriptCapture::for_checkout(repo.path(), repo.path());
        let user = |text: &str| event(WorkstreamEventKind::Message, "user", text, json!({}));
        let mut codex = user("<bash-stdout>codex text</bash-stdout>");
        codex.agent = AgentKind::Codex;
        let events = [
            user("<bash-input>cat notes.txt</bash-input>"),
            user("<bash-stdout>SECRET</bash-stdout><bash-stderr></bash-stderr>"),
            user("<local-command-stdout>output</local-command-stdout>"),
            user("<bash-stdout>cut by the size cap"),
            user("why does <bash-stdout>x</bash-stdout> appear?"),
            user("<bash-stdout>x</bash-stdout> and then a question"),
            codex,
        ];
        assert_eq!(
            kinds(&capture.decide(&events)),
            ["keep", "drop", "drop", "drop", "keep", "keep", "keep"]
        );
    }

    #[test]
    fn ledger_events_are_dropped_or_stripped_before_upload() {
        let repo = checkout(Some("docs/adr/**"));
        let capture = TranscriptCapture::for_checkout(repo.path(), repo.path());
        let malformed = event(
            WorkstreamEventKind::ToolCall,
            "assistant",
            "Read: {\"path\": 7, \"secret\": \"SENTINEL-ARG\"}",
            json!({"tool": "Read", "tool_use_id": "m", "parent_id": "p"}),
        );
        let kept = capture.filter_ledger_events(vec![
            read_call(&repo.path().join("docs/adr/1.md"), Some("adr")),
            result("SENTINEL-ADR", Some("adr")),
            malformed,
            result("SENTINEL-OUT", Some("m")),
            read_call(&repo.path().join("src/lib.rs"), Some("pub")),
            result("public code", Some("pub")),
        ]);
        let text = serde_json::to_string(&kept).unwrap();
        assert!(!text.contains("SENTINEL"), "{text}");
        assert!(text.contains("public code"), "control kept: {text}");
        assert_eq!(kept.len(), 4);
        assert_eq!(
            kept[0].content,
            "[file tool call withheld by the capture policy]"
        );
        assert_eq!(
            kept[1].content,
            "[file tool output withheld by the capture policy]"
        );
        assert_eq!(kept[1].metadata, json!({"tool_use_id": "m"}));
    }
}
