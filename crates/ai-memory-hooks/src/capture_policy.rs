//! Pure, IO-free capture policy evaluation for native hooks and server defense.

use ai_memory_core::AgentKind;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};

/// Maximum number of `ignore_paths` entries (entries, not bytes).
pub const MAX_IGNORE_PATTERNS: usize = 128;
/// Maximum Unicode scalar characters in one pattern.
pub const MAX_IGNORE_PATTERN_CHARS: usize = 1_024;
/// Marker readers must enforce this maximum byte read before parsing.
pub const MAX_MARKER_BYTES: usize = 64 * 1024;
/// Maximum Unicode scalar characters accepted in an extracted candidate path.
pub const MAX_CANDIDATE_PATH_CHARS: usize = 4_096;
/// Maximum direct path candidates accepted from one recognized file tool call.
pub const MAX_CAPTURE_CANDIDATES: usize = 32;
/// Maximum aggregate pattern-by-candidate scalar comparisons per inspection.
pub const MAX_MATCH_WORK: usize = 1_000_000;
/// Longest argv element kept whole as a path; longer ones are scripts.
const MAX_ARGV_PATH_CHARS: usize = 256;
const MAX_CALL_ID_CHARS: usize = 128;
const CAPTURE_PROTOCOL_VERSION: u8 = 1;

/// Minimal decoded `[capture]` marker configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CaptureConfig {
    /// Whole-path glob patterns to exclude.
    #[serde(default)]
    pub ignore_paths: Vec<String>,
}

/// Whether a repository is captured unless excluded, or only when it opts in.
///
/// This is the failure-mode switch requested in #446. Under [`Self::Denylist`]
/// — the historical behaviour and still the default — a repository with no
/// marker is captured, so forgetting a marker leaks. Under
/// [`Self::Allowlist`] the same omission captures nothing.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureMode {
    /// Absence of a marker means capture normally.
    #[default]
    Denylist,
    /// Absence of a marker means capture nothing at all.
    Allowlist,
}

/// Whether this repository may emit *any* lifecycle event, decided before the
/// per-event policy in [`CapturePolicy::inspect`] and before anything is
/// spooled.
///
/// Deliberately independent of the event kind. `inspect` is reached only for
/// tool events (`is_tool_event` in the CLI hook), so a gate expressed through
/// [`CaptureDisposition`] alone would leave `UserPromptSubmit`,
/// `SessionStart`/`SessionEnd`, and `Stop` bodies flowing while reporting the
/// repository as opted out — the precise false guarantee #446 is about. Prompt
/// text is the field that issue cares about most, so this must gate every
/// event or it gates nothing that matters.
#[must_use]
pub const fn repository_admits_capture(mode: CaptureMode, marker_present: bool) -> bool {
    match mode {
        CaptureMode::Denylist => true,
        CaptureMode::Allowlist => marker_present,
    }
}

/// Typed result of marker discovery and parsing, supplied by the IO-owning caller.
pub enum CaptureSource<'a> {
    /// No nearest marker exists.
    Absent,
    /// A complete marker parsed into the strict capture configuration.
    Parsed(&'a CaptureConfig),
    /// Discovery, bounded read, TOML parsing, or type validation failed.
    Invalid,
}

/// Local capture action.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureDisposition {
    /// Preserve the original event.
    Keep,
    /// Do not spool, queue, or transmit the event.
    Drop,
    /// Replace the event body with the strict metadata allowlist.
    MetadataOnly,
}

/// Resolution state of marker configuration.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyState {
    /// No marker or an explicitly empty list.
    Inactive,
    /// A complete valid pattern set is active.
    Active,
    /// Marker/configuration failed strict validation.
    Invalid,
}

/// Result of direct, schema-specific tool extraction.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExtractionState {
    /// Tool does not require path extraction.
    NotApplicable,
    /// Direct fields were extracted successfully.
    Extracted,
    /// A recognized file tool had absent, malformed, blank, or unusable fields.
    MissingOrMalformed,
    /// The agent payload did not use an explicitly supported adapter schema.
    #[default]
    UnsupportedSchema,
}

/// Canonical category used in the reserved protocol.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ToolFamily {
    /// Recognized direct file operation.
    File,
    /// Recognized search/list operation.
    SearchList,
    /// Explicitly known non-file operation.
    NonFile,
    /// Unknown or unsupported tool.
    #[default]
    Unknown,
}

/// Safe, closed-schema tool metadata suitable for an observation body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ToolObservationMetadata {
    pub(crate) tool_family: ToolFamily,
    pub(crate) tool_call_id: Option<String>,
}

/// Safe outcome class for a completed tool call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ToolOutcome {
    Success,
    Error,
    Unknown,
}

impl ToolOutcome {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Error => "error",
            Self::Unknown => "unknown",
        }
    }
}

/// Extracts only the closed, fixture-backed metadata schemas used for tool
/// observations. It intentionally does not reuse broad envelope fallbacks.
pub(crate) fn tool_observation_metadata(
    agent: AgentKind,
    raw: &Value,
    require_input: bool,
) -> Option<ToolObservationMetadata> {
    let object = raw.as_object()?;
    let (name, id) = match agent {
        // ZCode tool payloads carry Claude Code's snake_case aliases
        // (`tool_name`, `tool_use_id`, `tool_input`) alongside the native
        // camelCase — all captured live (engine v0.16.5, #512).
        // Native Codex 0.154 uses these same top-level fields, including
        // `tool_use_id` on both sides of a tool call.
        // Grok Build CLI posts Claude Code's snake_case aliases
        // (`tool_name` / `tool_input` / `tool_use_id`) on its tool hooks
        // alongside camelCase, so it shares this mapping (#931).
        // GitHub Copilot CLI, configured with PascalCase hook event names per
        // its own docs, emits the same Claude-compatible `tool_name`/
        // `tool_input` shape (#1040) — unverified against a real tool-call id
        // field, but `tool_use_id` extraction degrades to `None` gracefully
        // if absent.
        // Cursor's own hooks and its copies of the Claude Code hooks carry
        // `tool_name`/`tool_input`/`tool_use_id` (captured live, Cursor 3.24.9).
        AgentKind::ClaudeCode
        | AgentKind::CommandCode
        | AgentKind::Codex
        | AgentKind::Cursor
        | AgentKind::Grok
        | AgentKind::Grizzybot
        | AgentKind::Zcode
        | AgentKind::CopilotCli => (
            object.get("tool_name")?.as_str()?,
            object.get("tool_use_id").and_then(Value::as_str),
        ),
        AgentKind::OpenCode => (
            object.get("tool")?.as_str()?,
            object.get("callID").and_then(Value::as_str),
        ),
        // The Pi extension is generated from the OMP one (`build_pi_extension`),
        // so both post the same `tool` / `callID` / `args` payload.
        AgentKind::Pi | AgentKind::Omp => (
            object.get("tool")?.as_str()?,
            object.get("callID").and_then(Value::as_str),
        ),
        // Kiro v2/v3 tool hooks use `tool_name` + `tool_input`. Unknown payload
        // shapes fail safe to metadata-only under an active policy.
        AgentKind::KiroCli => (object.get("tool_name")?.as_str()?, None),
        AgentKind::AntigravityCli => (object.get("toolCall")?.get("name")?.as_str()?, None),
        AgentKind::Hermes => (
            object.get("tool_name")?.as_str()?,
            object
                .get("extra")
                .and_then(Value::as_object)
                .and_then(|extra| extra.get("tool_call_id"))
                .and_then(Value::as_str),
        ),
        // Pool (Poolside Agent CLI) tool hooks use snake_case `tool_name` +
        // `tool_input` (hooks api 1.0, verified against Poolside CLI v1.0.16);
        // no tool-call id is documented. Unknown payload shapes fail safe to
        // metadata-only under an active policy.
        AgentKind::Pool => (object.get("tool_name")?.as_str()?, None),
        _ => return None,
    };
    // PreToolUse needs a proven input shape. PostToolUse deliberately does
    // not: established adapters commonly omit inputs from their response.
    let has_args = !require_input
        || match agent {
            AgentKind::AntigravityCli => object.get("toolCall")?.get("args").is_some(),
            _ => object
                .get(
                    if matches!(
                        agent,
                        AgentKind::ClaudeCode
                            | AgentKind::CommandCode
                            | AgentKind::Codex
                            | AgentKind::Cursor
                            | AgentKind::Grok
                            | AgentKind::Hermes
                            | AgentKind::KiroCli
                            | AgentKind::Pool
                            | AgentKind::Zcode
                            | AgentKind::CopilotCli
                            | AgentKind::Grizzybot
                    ) {
                        "tool_input"
                    } else {
                        "args"
                    },
                )
                .is_some(),
        };
    has_args.then(|| ToolObservationMetadata {
        tool_family: agent_family(agent, name),
        tool_call_id: id.filter(|id| valid_call_id(id)).map(str::to_owned),
    })
}

/// Extracts an outcome only where the adapter protocol proves its meaning.
pub(crate) fn tool_observation_outcome(agent: AgentKind, raw: &Value) -> ToolOutcome {
    match agent {
        AgentKind::Pi | AgentKind::Omp => match raw.get("isError").and_then(Value::as_bool) {
            Some(true) => ToolOutcome::Error,
            Some(false) => ToolOutcome::Success,
            None => ToolOutcome::Unknown,
        },
        AgentKind::KiroCli => match raw
            .get("tool_response")
            .and_then(|response| response.get("success"))
            .and_then(Value::as_bool)
        {
            Some(true) => ToolOutcome::Success,
            Some(false) => ToolOutcome::Error,
            None => ToolOutcome::Unknown,
        },
        AgentKind::AntigravityCli
            if raw
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|error| !error.is_empty()) =>
        {
            ToolOutcome::Error
        }
        // ZCode fires `PostToolUseFailure` instead of `PostToolUse` when the
        // tool throws; that payload carries `error` (string) + `error_details`
        // (live-captured, #512). `exitCode` is deliberately not mapped to
        // Success: the same `tool_response` object carries `timedOut` /
        // `interrupted`, so exit code alone does not prove success.
        AgentKind::Zcode
            if raw
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|error| !error.is_empty()) =>
        {
            ToolOutcome::Error
        }
        // GitHub Copilot CLI's VS-Code-compatible `PostToolUse` carries
        // `tool_result.result_type` (#1040). Only `success` and an explicit
        // failure prove an outcome; any other value (a denied or rejected
        // call, a future type) stays unknown. `PostToolUseFailure` is aliased
        // to `PostToolUse`, so an `error` string with no `tool_result` is the
        // failure form, as for ZCode.
        AgentKind::CopilotCli => match raw
            .get("tool_result")
            .and_then(|result| result.get("result_type"))
            .and_then(Value::as_str)
        {
            Some("success") => ToolOutcome::Success,
            Some("failure" | "error") => ToolOutcome::Error,
            Some(_) => ToolOutcome::Unknown,
            None if raw
                .get("error")
                .and_then(Value::as_str)
                .is_some_and(|error| !error.is_empty()) =>
            {
                ToolOutcome::Error
            }
            None => ToolOutcome::Unknown,
        },
        // Hermes mirrors the call status in `extra.status` (`ok` | `error`,
        // set by `model_tools.py::_emit_post_tool_call_hook` from the same
        // result the model sees). `ok` proves success and a non-empty
        // `error_message` proves failure; anything else stays unknown.
        AgentKind::Hermes => {
            let status = raw
                .get("extra")
                .and_then(|extra| extra.get("status"))
                .and_then(Value::as_str);
            match status {
                Some("ok") => ToolOutcome::Success,
                Some("error") => ToolOutcome::Error,
                _ => ToolOutcome::Unknown,
            }
        }
        // Codex PostToolUse also fires for failed commands. Its native exec
        // response is output text, with no separate success/exit-code field;
        // neither the event nor arbitrary response JSON proves an outcome.
        _ => ToolOutcome::Unknown,
    }
}

/// Strict, bounded reserved protocol body.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CaptureProtocol {
    version: u8,
    disposition: CaptureDisposition,
    policy_state: PolicyState,
    tool_family: ToolFamily,
    path_count: u16,
    extraction_state: ExtractionState,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireProtocol {
    version: u8,
    disposition: CaptureDisposition,
    policy_state: PolicyState,
    tool_family: ToolFamily,
    path_count: u16,
    extraction_state: ExtractionState,
}

impl<'de> Deserialize<'de> for CaptureProtocol {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = WireProtocol::deserialize(deserializer)?;
        if wire.version != CAPTURE_PROTOCOL_VERSION {
            return Err(serde::de::Error::custom(
                "unsupported capture protocol version",
            ));
        }
        Ok(Self::new(
            wire.disposition,
            wire.policy_state,
            wire.tool_family,
            wire.path_count,
            wire.extraction_state,
        ))
    }
}

impl CaptureProtocol {
    fn new(
        disposition: CaptureDisposition,
        policy_state: PolicyState,
        tool_family: ToolFamily,
        path_count: u16,
        extraction_state: ExtractionState,
    ) -> Self {
        Self {
            version: CAPTURE_PROTOCOL_VERSION,
            disposition,
            policy_state,
            tool_family,
            path_count,
            extraction_state,
        }
    }
    /// Parses only the current protocol version and bounded field set.
    #[must_use]
    pub fn parse(value: &Value) -> Option<Self> {
        serde_json::from_value(value.clone()).ok()
    }
    /// Fixed wire protocol version.
    #[must_use]
    pub const fn version(&self) -> u8 {
        self.version
    }
    /// Chosen local action.
    #[must_use]
    pub const fn disposition(&self) -> CaptureDisposition {
        self.disposition
    }
    /// Resolved marker state.
    #[must_use]
    pub const fn policy_state(&self) -> PolicyState {
        self.policy_state
    }
    /// Canonical tool family.
    #[must_use]
    pub const fn tool_family(&self) -> ToolFamily {
        self.tool_family
    }
    /// Number of direct candidates, capped by the fixed `u16` wire type.
    #[must_use]
    pub const fn path_count(&self) -> u16 {
        self.path_count
    }
    /// Direct extraction result.
    #[must_use]
    pub const fn extraction_state(&self) -> ExtractionState {
        self.extraction_state
    }
}

/// Compiled policy plus its state. Construct with [`CapturePolicy::resolve`].
#[derive(Clone, Debug)]
pub struct CapturePolicy {
    state: PolicyState,
    patterns: Vec<CompiledPattern>,
    home: Option<String>,
    /// The marker directory, where git resolves a `REV:path` object name
    /// when the repository root holds the marker.
    root: Option<String>,
    /// POSIX paths compare without regard to ASCII case, as on macOS, whose
    /// default filesystem reads `NOTES/X.md` as `notes/x.md`.
    fold_case: bool,
}

#[derive(Clone, Debug)]
struct CompiledPattern {
    path: String,
    flavor: Flavor,
    directory_base: Option<String>,
    /// Whole segments before the first glob; a matching path must start here.
    literal_prefix: String,
}
impl CompiledPattern {
    /// Upper bound on `glob_match` steps for one candidate, directory glob included.
    fn match_cost(&self, candidate: &str) -> usize {
        let pattern = self.path.chars().count()
            + self
                .directory_base
                .as_deref()
                .map_or(0, |base| base.chars().count());
        pattern.saturating_mul(candidate.chars().count())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flavor {
    Posix,
    Windows,
}

/// Safe inspection result; it never retains raw arguments, paths, or arbitrary tool names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureDecision {
    protocol: CaptureProtocol,
    identity: CanonicalTool,
    call_id: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum CanonicalTool {
    File,
    SearchList,
    NonFile,
    Unknown,
}

impl CaptureDecision {
    /// Protocol clients send as `_ai_memory_capture` when the policy is active/invalid.
    #[must_use]
    pub const fn protocol(&self) -> &CaptureProtocol {
        &self.protocol
    }
}

impl CapturePolicy {
    /// This policy's resolution state: whether a marker was absent, parsed
    /// into an active pattern set, or present but rejected (bad TOML, a
    /// `[capture]` shape `read_capture_config` doesn't accept, or a pattern
    /// `compile` rejects, e.g. an unsupported glob). `Invalid` already fails
    /// closed in [`Self::inspect`] — this accessor exists for diagnostics
    /// (`ai-memory doctor`) that want to say *why* without re-deriving it
    /// from scratch or trusting a parse-only check that misses the
    /// `compile`-rejected case.
    #[must_use]
    pub const fn state(&self) -> PolicyState {
        self.state
    }

    /// Resolves a marker config atomically. Empty parsed configuration is inactive.
    /// `home_dir` is required only when at least one pattern starts with `~/`.
    #[must_use]
    pub fn resolve(source: CaptureSource<'_>, marker_dir: &str, home_dir: Option<&str>) -> Self {
        Self::resolve_for_host(source, marker_dir, home_dir, cfg!(target_os = "macos"))
    }
    /// [`Self::resolve`] for a host whose filesystem does (`fold_case`) or
    /// does not ignore case. Only the client holds the patterns, so the
    /// server's re-inspection never depends on this.
    fn resolve_for_host(
        source: CaptureSource<'_>,
        marker_dir: &str,
        home_dir: Option<&str>,
        fold_case: bool,
    ) -> Self {
        let config = match source {
            CaptureSource::Absent => return Self::inactive(),
            CaptureSource::Invalid => {
                return Self {
                    state: PolicyState::Invalid,
                    patterns: Vec::new(),
                    home: None,
                    root: None,
                    fold_case,
                };
            }
            CaptureSource::Parsed(config) => config,
        };
        if config.ignore_paths.is_empty() {
            return Self::inactive();
        }
        let compiled = compile(config, marker_dir, home_dir);
        match compiled {
            Ok(patterns) => Self {
                state: PolicyState::Active,
                patterns,
                home: home_dir.and_then(|dir| normalize_root(dir).ok()),
                root: normalize_root(marker_dir).ok(),
                fold_case,
            },
            Err(_) => Self {
                state: PolicyState::Invalid,
                patterns: Vec::new(),
                home: None,
                root: None,
                fold_case,
            },
        }
    }
    fn inactive() -> Self {
        Self {
            state: PolicyState::Inactive,
            patterns: Vec::new(),
            home: None,
            root: None,
            fold_case: false,
        }
    }
    /// Evaluates every policy state using only direct fixture-backed schemas.
    #[must_use]
    pub fn inspect(&self, agent: AgentKind, raw: &Value, cwd: &str) -> CaptureDecision {
        let extracted = extract(agent, raw);
        let (disposition, extraction) = match self.state {
            PolicyState::Inactive => (CaptureDisposition::Keep, extracted.state),
            // A broken marker cannot prove a shell command's arguments miss
            // every ignored path, so it fails closed like a file tool, readable
            // or not.
            PolicyState::Invalid
                if extracted.family == ToolFamily::File
                    || extracted.shell
                    || extracted.output_reader =>
            {
                (CaptureDisposition::MetadataOnly, extracted.state)
            }
            PolicyState::Invalid => (CaptureDisposition::Keep, extracted.state),
            // A tool that reads back an earlier command's output names no
            // path of its own, so nothing proves that command missed every
            // ignored path.
            PolicyState::Active if extracted.output_reader => {
                (CaptureDisposition::MetadataOnly, extracted.state)
            }
            PolicyState::Active => match extracted.family {
                ToolFamily::SearchList => {
                    (CaptureDisposition::Drop, ExtractionState::NotApplicable)
                }
                ToolFamily::File => match extracted.paths.as_ref() {
                    None => (
                        CaptureDisposition::MetadataOnly,
                        ExtractionState::MissingOrMalformed,
                    ),
                    Some(paths)
                        if paths
                            .iter()
                            .any(|path| normalize_candidate(path, cwd).is_none()) =>
                    {
                        (
                            CaptureDisposition::MetadataOnly,
                            ExtractionState::MissingOrMalformed,
                        )
                    }
                    Some(paths) => {
                        let matched = if extracted.globs {
                            self.match_globs(paths, cwd)
                        } else {
                            self.match_paths(paths, cwd)
                        };
                        match matched {
                            Ok(true) => (CaptureDisposition::Drop, ExtractionState::Extracted),
                            Ok(false) => (CaptureDisposition::Keep, ExtractionState::Extracted),
                            Err(()) => (
                                CaptureDisposition::MetadataOnly,
                                ExtractionState::MissingOrMalformed,
                            ),
                        }
                    }
                },
                // A shell command has no path field, but its arguments can
                // still name an ignored file whose content lands in the
                // output (`cat docs/adr/*.md`). An exhausted match budget
                // fails closed like an unprovable file candidate.
                ToolFamily::NonFile => match extracted.command.as_ref().map(|command| {
                    let base = match extracted.workdir.as_deref() {
                        Some(dir) if is_absolute(dir) => dir.to_owned(),
                        // `join` would turn an unusable cwd into `/dir`.
                        Some(dir) if is_absolute(cwd) => join(cwd, dir),
                        _ => cwd.to_owned(),
                    };
                    // PowerShell's backquote and cmd's caret escape the next
                    // character, so their commands also get a reading
                    // without them.
                    let split =
                        command.split(extracted.powershell || flavor_of(cwd) == Flavor::Windows);
                    self.match_command(&split.words, &base)
                        .map(|matched| (matched, split.opaque))
                }) {
                    Some(Ok((true, _)) | Err(())) => (CaptureDisposition::Drop, extracted.state),
                    // Syntax the matcher does not evaluate (brace expansion,
                    // bracket classes, zsh groups, encoded escapes) and inline
                    // interpreter code prove nothing either way.
                    Some(Ok((false, true))) => (CaptureDisposition::MetadataOnly, extracted.state),
                    Some(Ok((false, false))) | None => (CaptureDisposition::Keep, extracted.state),
                },
                _ => (CaptureDisposition::Keep, extracted.state),
            },
        };
        CaptureDecision {
            protocol: CaptureProtocol::new(
                disposition,
                self.state,
                extracted.family,
                extracted
                    .paths
                    .as_ref()
                    .map_or(0, |p| p.len().min(u16::MAX as usize) as u16),
                extraction,
            ),
            identity: canonical(extracted.family),
            call_id: extracted.call_id,
        }
    }
    fn match_paths(&self, paths: &[String], cwd: &str) -> Result<bool, ()> {
        let candidates: Option<Vec<_>> = paths
            .iter()
            .map(|path| normalize_candidate(path, cwd))
            .collect();
        let candidates = candidates.ok_or(())?;
        let mut work = 0_usize;
        for candidate in candidates {
            for pattern in self
                .patterns
                .iter()
                .filter(|pattern| pattern.flavor == candidate.flavor)
            {
                work = work
                    .checked_add(pattern.match_cost(&candidate.path))
                    .ok_or(())?;
                if work > MAX_MATCH_WORK {
                    return Err(());
                }
                if glob_match(
                    &pattern.path,
                    &candidate.path,
                    pattern.directory_base.as_deref(),
                    pattern.flavor == Flavor::Windows || self.fold_case,
                ) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    /// Lexically matches every path-like shell argument. Nothing is expanded
    /// or executed: variables, command substitution, and `cd` are not
    /// followed, so this narrows the leak rather than closing every alias.
    fn match_command(&self, words: &[String], cwd: &str) -> Result<bool, ()> {
        let mut work = 0_usize;
        for word in words {
            for (argument, from_root) in shell_arguments(word) {
                let home = argument
                    .strip_prefix("~/")
                    .or_else(|| argument.strip_prefix("~\\"));
                // `~+` is the working directory.
                let expanded = if let (Some(rest), Some(home)) = (home, self.home.as_deref()) {
                    join(home, rest)
                } else if argument == "~+" {
                    cwd.to_owned()
                } else if let Some(rest) = argument.strip_prefix("~+/") {
                    join(cwd, rest)
                } else {
                    argument
                };
                // Git reads `REV:path` from the repository root, which only
                // the marker directory approximates; try the cwd as well.
                let root = (from_root && !is_absolute(&expanded))
                    .then_some(self.root.as_deref())
                    .flatten();
                for base in std::iter::once(cwd).chain(root) {
                    // Not a usable path (drive-relative, oversized, bad cwd).
                    let Some(candidate) = normalize_candidate(&expanded, base) else {
                        continue;
                    };
                    if self.match_pattern_candidate(&candidate, false, &mut work)? {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
    /// Matches the path arguments of Gemini CLI's `read_many_files`, which are
    /// globs expanded case-insensitively (`nocase`) with `**` recursing into
    /// any depth. A literal path matches like a file tool's; a glob drops when
    /// it could expand into an ignored path, the same over-approximation
    /// shell globs get. Glob syntax the matcher cannot evaluate (classes,
    /// braces, extglobs, negation) fails closed.
    fn match_globs(&self, paths: &[String], cwd: &str) -> Result<bool, ()> {
        let mut work = 0_usize;
        for path in paths {
            if path.contains(['[', ']', '{', '}', '(', ')', '!']) {
                return Err(());
            }
            let candidate = normalize_candidate(path, cwd).ok_or(())?;
            if self.match_pattern_candidate(&candidate, true, &mut work)? {
                return Ok(true);
            }
        }
        Ok(false)
    }
    /// Whether one normalized candidate, literal or glob, names or can expand
    /// into an ignored path. `tool_glob` selects `read_many_files` expansion
    /// (case-insensitive, recursive `**`) over a shell's; Windows, and a
    /// case-folding host, always ignore case.
    fn match_pattern_candidate(
        &self,
        candidate: &Normalized,
        tool_glob: bool,
        work: &mut usize,
    ) -> Result<bool, ()> {
        let insensitive = tool_glob || candidate.flavor == Flavor::Windows || self.fold_case;
        let is_glob = candidate.path.contains(['*', '?']);
        // `read_many_files` reads a directory entry (`docs`, `.`) as
        // `entry/**`, and nothing here can tell a directory from a file.
        let directory = tool_glob && !is_glob;
        for pattern in self
            .patterns
            .iter()
            .filter(|pattern| pattern.flavor == candidate.flavor)
        {
            // Linear and bounded by the candidate itself; only the
            // quadratic matches below are charged to the budget.
            let under_prefix =
                starts_with_chars(&candidate.path, &pattern.literal_prefix, insensitive);
            if !under_prefix && !is_glob && !directory {
                continue;
            }
            *work = work
                .checked_add(pattern.match_cost(&candidate.path))
                .and_then(|work| {
                    work.checked_add(if directory {
                        pattern.match_cost(&candidate.path)
                    } else {
                        0
                    })
                })
                .ok_or(())?;
            if *work > MAX_MATCH_WORK {
                return Err(());
            }
            // Treat `**` as recursive for shell globs too: bash leaves it a
            // single segment, but zsh (and bash with `globstar`) recurse, and
            // an over-eager drop is the safe side of a capture exclusion.
            let reaches = globstar_reaches;
            if (under_prefix
                && glob_match(
                    &pattern.path,
                    &candidate.path,
                    pattern.directory_base.as_deref(),
                    insensitive,
                ))
                || (is_glob && reaches(&candidate.path, &pattern.literal_prefix, insensitive))
                || (directory && glob_matches_below(&pattern.path, &candidate.path, insensitive))
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// Constructs a metadata-only replacement from scalar allowlisted envelope values and a decision.
#[must_use]
pub fn metadata_only_body(
    session_id: Option<&str>,
    cwd: Option<&str>,
    decision: &CaptureDecision,
) -> Value {
    let mut body = Map::new();
    if let Some(value) = session_id {
        body.insert("session_id".into(), Value::String(value.into()));
    }
    if let Some(value) = cwd {
        body.insert("cwd".into(), Value::String(value.into()));
    }
    body.insert("tool_family".into(), json!(decision.protocol.tool_family()));
    body.insert("tool_name".into(), json!(decision.identity));
    if let Some(value) = &decision.call_id {
        body.insert("tool_call_id".into(), Value::String(value.clone()));
    }
    body.insert("_ai_memory_capture".into(), json!(decision.protocol));
    Value::Object(body)
}

/// Rebuilds the payload shape [`CapturePolicy::inspect`] and
/// [`absolute_tool_location_paths`] read for `agent` from a tool call recorded
/// outside a hook — a native transcript event replayed by `backfill` or the
/// `ai-memory run` ledger — so the import applies the live decision table
/// rather than a second matcher. `args: None` means the arguments could not be
/// recovered, which a file tool treats as malformed.
///
/// A transcript can name a tool differently from the hook: Codex records its
/// shell tool as `exec_command` with `{cmd, workdir}`, which reaches hooks as
/// `Bash`. Such a call is rebuilt under the hook's name and fields, so the
/// import classifies it as the live hook does.
#[must_use]
pub fn tool_call_payload(
    agent: AgentKind,
    name: &str,
    args: Option<&Value>,
    call_id: Option<&str>,
) -> Value {
    let args = args.cloned().unwrap_or(Value::Null);
    let (name, args) = hook_tool_call(agent, name, args);
    let mut payload = match agent {
        AgentKind::AntigravityCli => json!({"toolCall": {"name": name, "args": args}}),
        AgentKind::Zero => json!({"toolName": name, "input": args}),
        AgentKind::OpenCode | AgentKind::Omp | AgentKind::Pi | AgentKind::OpenClaw => {
            json!({"tool": name, "args": args})
        }
        _ => json!({"tool_name": name, "tool_input": args}),
    };
    if let (Some(id), Some(object)) = (call_id, payload.as_object_mut()) {
        object.insert("tool_use_id".into(), Value::String(id.into()));
    }
    payload
}

/// The name and arguments the live hook receives for a call a transcript
/// records under its own name. Unreadable arguments stay unreadable.
fn hook_tool_call(agent: AgentKind, name: &str, args: Value) -> (&str, Value) {
    if agent == AgentKind::Codex && name.eq_ignore_ascii_case("exec_command") {
        let Value::Object(recorded) = args else {
            return ("Bash", args);
        };
        let mut input = Map::new();
        for (from, to) in [("cmd", "command"), ("workdir", "workdir")] {
            if let Some(value) = recorded.get(from) {
                input.insert(to.into(), value.clone());
            }
        }
        return ("Bash", Value::Object(input));
    }
    (name, args)
}

#[derive(Default)]
struct Extracted<'a> {
    family: ToolFamily,
    paths: Option<Vec<String>>,
    /// A shell tool's command, not yet split into words.
    command: Option<ShellCommand<'a>>,
    /// A command-running tool, even when its command is unreadable.
    /// `web_search` is non-file but runs nothing.
    shell: bool,
    workdir: Option<String>,
    /// A search/list tool's explicit directory or file argument. Used only to
    /// locate where the tool ran; capture policy drops search output under an
    /// active marker regardless of it.
    search_paths: Option<Vec<String>>,
    /// The paths are globs (Gemini CLI `read_many_files`), not literal files.
    globs: bool,
    /// The tool reads back output of an earlier command (`BashOutput`).
    output_reader: bool,
    /// Claude Code's `PowerShell` tool.
    powershell: bool,
    call_id: Option<String>,
    state: ExtractionState,
}

fn extract(agent: AgentKind, raw: &Value) -> Extracted<'_> {
    let Some(object) = raw.as_object() else {
        return Extracted::default();
    };
    let extracted = match agent {
        AgentKind::AntigravityCli => object
            .get("toolCall")
            .and_then(Value::as_object)
            .and_then(|call| Some((call.get("name")?.as_str()?, call.get("args")))),
        AgentKind::Grok => object
            .get("tool_name")
            .or_else(|| object.get("toolName"))
            .and_then(Value::as_str)
            .map(|name| {
                (
                    name,
                    object.get("tool_input").or_else(|| object.get("toolInput")),
                )
            }),
        AgentKind::Zero => object
            .get("toolName")
            .and_then(Value::as_str)
            .map(|name| (name, object.get("input"))),
        AgentKind::ClaudeCode
        | AgentKind::CommandCode
        | AgentKind::Codex
        | AgentKind::Cursor
        | AgentKind::GeminiCli
        | AgentKind::Devin
        | AgentKind::Hermes
        | AgentKind::KiroCli
        | AgentKind::Pool
        // ZCode mirrors Claude Code's snake_case `tool_name`/`tool_input`
        // aliases on every tool event (live-captured, #512).
        // GrizzyBot posts the same Claude Code-shaped payload from its agent
        // loop.
        | AgentKind::Grizzybot
        | AgentKind::Zcode
        // GitHub Copilot CLI, PascalCase-configured for the Claude-compatible
        // payload shape (#1040).
        | AgentKind::CopilotCli => object
            .get("tool_name")
            .and_then(Value::as_str)
            .map(|name| (name, object.get("tool_input"))),
        AgentKind::OpenCode | AgentKind::Omp | AgentKind::Pi | AgentKind::OpenClaw => object
            .get("tool")
            .and_then(Value::as_str)
            .map(|name| (name, object.get("args"))),
        _ => None,
    };
    let Some((name, args)) = extracted else {
        return Extracted::default();
    };
    let family = agent_family(agent, name);
    let paths = (family == ToolFamily::File)
        .then(|| args.and_then(|value| extract_paths(name, value)))
        .flatten();
    let command = (family == ToolFamily::NonFile)
        .then(|| args.and_then(shell_command))
        .flatten();
    let search_paths = (family == ToolFamily::SearchList)
        .then(|| args.and_then(|value| direct_paths(value.as_object()?)))
        .flatten();
    // OpenCode `bash`, OpenClaw `exec` and Codex `shell` run in `workdir`
    // when given, Gemini CLI `run_shell_command` in `dir_path` (`directory`
    // before it was renamed), and Antigravity `run_command` in `Cwd`, so
    // relative arguments resolve from there.
    let shell = family == ToolFamily::NonFile && !name.eq_ignore_ascii_case("web_search");
    let workdir_keys: &[&str] = if agent == AgentKind::AntigravityCli {
        &["Cwd"]
    } else {
        &["workdir", "dir_path", "directory"]
    };
    let workdir = command
        .as_ref()
        .and_then(|_| {
            let args = args?;
            workdir_keys.iter().find_map(|key| args.get(*key))?.as_str()
        })
        .filter(|dir| !dir.trim().is_empty())
        .map(str::to_owned);
    let state = if family == ToolFamily::File && paths.is_none() {
        ExtractionState::MissingOrMalformed
    } else {
        ExtractionState::Extracted
    };
    let call_id = [
        "tool_use_id",
        "toolUseId",
        "tool_call_id",
        "toolCallId",
        "call_id",
        "callId",
        "callID",
    ]
    .iter()
    .find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_str)
            .filter(|id| valid_call_id(id))
            .map(str::to_owned)
    });
    Extracted {
        family,
        paths,
        command,
        shell,
        workdir,
        search_paths,
        globs: name.eq_ignore_ascii_case("read_many_files"),
        output_reader: reads_earlier_output(name),
        powershell: name.eq_ignore_ascii_case("powershell"),
        call_id,
        state,
    }
}

/// [`family`] plus tool names only one agent is documented to use, so a
/// same-named tool of another agent keeps its current classification.
fn agent_family(agent: AgentKind, name: &str) -> ToolFamily {
    if agent == AgentKind::CopilotCli {
        // GitHub Copilot CLI's runtime tool names: `view` reads a file or
        // directory, `create` writes one, `str_replace_editor` edits, each
        // naming its target in `path`; `rg` is its `grep` alias.
        match name.to_ascii_lowercase().as_str() {
            "view" | "create" | "str_replace_editor" => return ToolFamily::File,
            "rg" => return ToolFamily::SearchList,
            _ => {}
        }
    }
    family(name)
}

/// Tools that return output of a command started by an earlier call: Claude
/// Code `BashOutput`/`TaskOutput`, Codex `write_stdin`, Hermes and OpenClaw
/// `process`, Copilot CLI `read_bash`/`write_bash` and their PowerShell twins.
/// That command's own event may have been dropped, so its output must not
/// come back through these.
fn reads_earlier_output(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "bashoutput"
            | "taskoutput"
            | "write_stdin"
            | "process"
            | "read_bash"
            | "write_bash"
            | "read_powershell"
            | "write_powershell"
    )
}

fn family(name: &str) -> ToolFamily {
    match name.to_ascii_lowercase().as_str() {
        "read"
        | "write"
        | "edit"
        | "apply_patch"
        | "notebookedit"
        | "notebook_edit"
        | "create_file"
        | "delete_file"
        | "remove"
        | "rename_file"
        | "move_file"
        | "multi_edit"
        | "multiedit"
        | "replace"
        | "replace_all"
        | "view_file"
        | "replace_file_content"
        | "multi_replace_file_content"
        | "write_to_file"
        | "fs_read"
        | "fs_write" => ToolFamily::File,
        "read_file" | "write_file" | "edit_file" | "patch" => ToolFamily::File,
        // Gemini CLI reads every file its `include` globs expand to.
        "read_many_files" => ToolFamily::File,
        "search" | "grep" | "glob" | "find" | "list" | "ls" | "list_files" | "read_dir"
        | "list_dir" | "grep_search" | "search_files" | "find_by_name" => ToolFamily::SearchList,
        // Gemini CLI's directory listing, and the pre-rename name of its
        // `grep_search` that it still accepts as an alias.
        "list_directory" | "search_file_content" => ToolFamily::SearchList,
        "bash" | "shell" | "shell_command" | "exec" | "execute" | "run_command" | "web_search"
        | "search_web" | "manage_task" | "manage_subagents" | "terminal" | "execute_bash"
        | "execute_cmd" => ToolFamily::NonFile,
        // Claude Code on Windows exposes `PowerShell` beside `Bash`, with the
        // same `tool_input.command` field (#1186).
        "powershell" => ToolFamily::NonFile,
        // Gemini CLI's shell tool, `{command, dir_path?}`.
        "run_shell_command" => ToolFamily::NonFile,
        // Hermes Agent tool surface (agent/shell_hooks.py envelope; tool
        // names from model_tools registry, live-captured 2026-10-07). These
        // execute code, drive a browser, or fetch the web — none touch
        // workspace file paths, so they are non-file rather than unknown:
        // family `unknown` drops the tool output from observation bodies.
        "execute_code" | "browser_exec" | "browser_navigate" | "web_extract" | "web_fetch"
        | "delegate_task" => ToolFamily::NonFile,
        _ => ToolFamily::Unknown,
    }
}
fn canonical(family: ToolFamily) -> CanonicalTool {
    match family {
        ToolFamily::File => CanonicalTool::File,
        ToolFamily::SearchList => CanonicalTool::SearchList,
        ToolFamily::NonFile => CanonicalTool::NonFile,
        ToolFamily::Unknown => CanonicalTool::Unknown,
    }
}
pub(crate) fn valid_call_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().count() <= MAX_CALL_ID_CHARS
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
}

fn extract_paths(name: &str, args: &Value) -> Option<Vec<String>> {
    let object = args.as_object()?;
    let mut paths = direct_paths(object).unwrap_or_default();
    if matches!(
        name.to_ascii_lowercase().as_str(),
        "multi_edit" | "multiedit" | "replace_all"
    ) {
        let entries = object
            .get("edits")
            .or_else(|| object.get("replacements"))?
            .as_array()?;
        if entries.is_empty() {
            return None;
        }
        if entries.len() > MAX_CAPTURE_CANDIDATES {
            return None;
        }
        for entry in entries {
            let paths_in_entry = direct_paths(entry.as_object()?)?;
            if paths.len().checked_add(paths_in_entry.len())? > MAX_CAPTURE_CANDIDATES {
                return None;
            }
            paths.extend(paths_in_entry);
        }
    }
    // Gemini CLI `read_many_files` names its globs in `include` (`paths`,
    // which `direct_paths` reads, before a rename). `exclude` only narrows
    // the read, so it is not a candidate.
    if name.eq_ignore_ascii_case("read_many_files")
        && let Some(include) = object.get("include")
    {
        let entries = include.as_array()?;
        if paths.len().checked_add(entries.len())? > MAX_CAPTURE_CANDIDATES {
            return None;
        }
        for entry in entries {
            paths.push(entry.as_str()?.to_owned());
        }
    }
    if matches!(name.to_ascii_lowercase().as_str(), "read" | "fs_read")
        && let Some(operations) = object.get("operations")
    {
        let entries = operations.as_array()?;
        if entries.is_empty() || entries.len() > MAX_CAPTURE_CANDIDATES {
            return None;
        }
        for entry in entries {
            let paths_in_entry = direct_paths(entry.as_object()?)?;
            if paths.len().checked_add(paths_in_entry.len())? > MAX_CAPTURE_CANDIDATES {
                return None;
            }
            paths.extend(paths_in_entry);
        }
    }
    (!paths.is_empty()
        && paths.iter().all(|path| {
            !path.trim().is_empty() && path.chars().count() <= MAX_CANDIDATE_PATH_CHARS
        }))
    .then_some(paths)
}

/// Return the bounded, normalized absolute paths that locate where a
/// recognized tool call ran, in the path's own separator style: a file tool's
/// targets, a search/list tool's explicit directory, or a shell command's
/// `workdir`.
///
/// Native hook clients use this only to notice a tool call that operates in
/// another checkout while the harness keeps reporting a different cwd — a
/// subagent's parent session (#932), or Hermes' process cwd (#1199).
/// Relative paths are intentionally excluded: joining them to any cwd other
/// than the payload's would be ambiguous, and they cannot independently prove
/// a cross-project target. Free-form command text is never parsed for a
/// location; only the structured `workdir` field counts. Unknown schemas and
/// mixed/oversized inputs fail closed with `None`, matching capture-policy
/// extraction.
#[must_use]
pub fn absolute_tool_location_paths(
    agent: AgentKind,
    raw: &Value,
    cwd: &str,
) -> Option<Vec<String>> {
    let extracted = extract(agent, raw);
    let paths = match extracted.family {
        ToolFamily::File => extracted.paths?,
        ToolFamily::SearchList => extracted.search_paths?,
        ToolFamily::NonFile => vec![extracted.workdir?],
        ToolFamily::Unknown => return None,
    };
    paths
        .iter()
        .map(|path| {
            is_absolute(path)
                .then(|| normalize_candidate(path, cwd))
                .flatten()
                .map(|candidate| match candidate.flavor {
                    // Matching works on `/`-separated paths, but this path
                    // leaves the policy module: the hook sends its directory
                    // as the event's `cwd`. It keeps the native style
                    // (`C:\…`, like every other cwd the hook forwards) so
                    // the server never sees two spellings of one checkout;
                    // segment normalization (`.`/`..`) still applies.
                    Flavor::Windows => candidate.path.replace('/', "\\"),
                    Flavor::Posix => candidate.path,
                })
        })
        .collect()
}

fn direct_paths(object: &Map<String, Value>) -> Option<Vec<String>> {
    let mut paths = Vec::new();
    for key in [
        "file_path",
        "filePath",
        "path",
        "absolute_path",
        "AbsolutePath",
        "notebook_path",
        "TargetFile",
    ] {
        if let Some(value) = object.get(key) {
            if paths.len() == MAX_CAPTURE_CANDIDATES {
                return None;
            }
            paths.push(value.as_str()?.to_owned());
        }
    }
    if let Some(values) = object.get("paths") {
        if values.as_array()?.len() > MAX_CAPTURE_CANDIDATES
            || paths.len().checked_add(values.as_array()?.len())? > MAX_CAPTURE_CANDIDATES
        {
            return None;
        }
        for value in values.as_array()? {
            paths.push(value.as_str()?.to_owned());
        }
    }
    (!paths.is_empty()).then_some(paths)
}

/// A shell tool's command as given, split into words on demand: `extract` runs
/// for every tool event and most never need them.
enum ShellCommand<'a> {
    Line(&'a str),
    Argv(Vec<&'a str>),
}

/// A shell tool's words, plus whether any of them holds syntax the lexical
/// matcher does not evaluate, which makes a miss unprovable.
#[derive(Default)]
struct ShellWords {
    words: Vec<String>,
    opaque: bool,
}

impl ShellCommand<'_> {
    #[cfg(test)]
    fn words(&self) -> Vec<String> {
        self.split(false).words
    }

    /// Every word, and again every word inside a word that holds more than
    /// one (`bash -c 'cat x'`, `"$(cat x)"`), down to [`MAX_SHELL_NESTING`]
    /// levels. A script nested deeper is opaque.
    ///
    /// An argument vector (Codex exec) keeps each element as one word
    /// (`private notes/x.md`) and also tokenizes it, so a `bash -lc "<script>"`
    /// element is read like any script. Joining the elements instead would
    /// re-split paths with spaces and let one stray quote swallow the rest.
    ///
    /// `windows` also reads the command without PowerShell and cmd escapes
    /// ([`without_windows_escapes`]).
    fn split(&self, windows: bool) -> ShellWords {
        let mut split = ShellWords::default();
        match self {
            Self::Line(command) => {
                split.opaque = has_unevaluated_expansion(command);
                push_nested_words(&shell_words(command), MAX_SHELL_NESTING, &mut split);
                if let Some(plain) = windows.then(|| without_windows_escapes(command)).flatten() {
                    push_nested_words(&shell_words(&plain), MAX_SHELL_NESTING, &mut split);
                }
            }
            Self::Argv(items) => {
                // Each element is scanned as a script too, the same
                // over-approximation its words make.
                split.opaque = items.iter().any(|item| has_unevaluated_expansion(item))
                    || runs_inline_code(items);
                for item in items {
                    push_argv_item(item, &mut split);
                    if let Some(plain) = windows.then(|| without_windows_escapes(item)).flatten() {
                        push_argv_item(&plain, &mut split);
                    }
                }
            }
        }
        // `~-`, `~N` and `~name` name a directory only the running shell
        // knows, so a path under one proves nothing either way.
        split.opaque |= split.words.iter().any(|word| {
            shell_arguments(word)
                .iter()
                .any(|(argument, _)| names_unknown_directory(argument))
        });
        split
    }
}

fn push_argv_item(item: &str, split: &mut ShellWords) {
    let tokens = shell_words(item);
    if tokens.len() == 1 && tokens[0] == item {
        push_word(item, split);
        return;
    }
    // Kept whole, a long script would exhaust the match budget.
    if item.chars().count() <= MAX_ARGV_PATH_CHARS {
        split.words.push(item.to_owned());
    }
    push_nested_words(&tokens, MAX_SHELL_NESTING - 1, split);
}

/// `command` without PowerShell's backquote and cmd's caret escapes, each of
/// which stands for the character after it (`secr`et.md`, `secr^et.md`).
fn without_windows_escapes(command: &str) -> Option<String> {
    if !command.contains(['`', '^']) {
        return None;
    }
    let mut plain = String::with_capacity(command.len());
    let mut chars = command.chars();
    while let Some(c) = chars.next() {
        if matches!(c, '`' | '^') {
            plain.extend(chars.next());
        } else {
            plain.push(c);
        }
    }
    Some(plain)
}

/// Whether an argument starts with a tilde prefix whose directory only the
/// running shell knows: `~-` (the previous directory), `~N`/`~+N` (the
/// directory stack) or `~name` (another account's home). `~`, `~/` and `~+`
/// (the working directory) are resolved instead.
fn names_unknown_directory(argument: &str) -> bool {
    let Some(rest) = argument.strip_prefix('~') else {
        return false;
    };
    let prefix = rest.split(['/', '\\']).next().unwrap_or(rest);
    !prefix.is_empty() && prefix != "+"
}

/// How many quoting levels a script inside a word is re-split.
const MAX_SHELL_NESTING: usize = 4;

fn push_nested_words(words: &[String], depth: usize, split: &mut ShellWords) {
    if runs_inline_code(words) {
        split.opaque = true;
    }
    for (index, word) in words.iter().enumerate() {
        push_word(word, split);
        let tokens = shell_words(word);
        if tokens.len() == 1 && tokens[0] == *word {
            continue;
        }
        let Some(depth) = depth.checked_sub(1) else {
            split.opaque = true;
            continue;
        };
        // Only a script a shell runs expands its braces and classes; a
        // quoted message or pattern (`git log --format='%h {a,b}'`) does not.
        if (word.contains("$(") || word.contains('`') || is_shell_script(words, index))
            && has_unevaluated_expansion(word)
        {
            split.opaque = true;
        }
        push_nested_words(&tokens, depth, split);
    }
}

/// Adds a word, and its POSIX reading when it differs: outside quotes a
/// shell drops the backslash of `\o`, but [`shell_words`] keeps it so a
/// Windows path keeps its separators. A word with a parameter default
/// (`${X:-docs/x.md}`) also adds the reading with the operand in its place.
fn push_word(word: &str, split: &mut ShellWords) {
    split.words.push(word.to_owned());
    if let Some(operands) = parameter_operands(word) {
        split.words.push(operands);
    }
    if word.contains('\\') {
        let mut plain = String::with_capacity(word.len());
        let mut chars = word.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                plain.extend(chars.next());
            } else {
                plain.push(c);
            }
        }
        if plain != word {
            split.words.push(plain);
        }
    }
}

/// `word` with each `${NAME:-word}`, `${NAME:=word}` or `${NAME:+word}`
/// (or the form without `:`) replaced by its operand, which the shell
/// substitutes when `NAME` is unset (set, for `+`). Nested operands are
/// replaced down to [`MAX_SHELL_NESTING`] levels.
fn parameter_operands(word: &str) -> Option<String> {
    let mut current = replace_parameter_operands(word)?;
    for _ in 1..MAX_SHELL_NESTING {
        match replace_parameter_operands(&current) {
            Some(next) => current = next,
            None => break,
        }
    }
    Some(current)
}

fn replace_parameter_operands(word: &str) -> Option<String> {
    let mut output = String::new();
    let mut rest = word;
    let mut changed = false;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let body = &rest[start + 2..];
        let name = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(body.len());
        let after = &body[name..];
        let operator =
            if after.starts_with(":-") || after.starts_with(":=") || after.starts_with(":+") {
                2
            } else if after.starts_with(['-', '=', '+']) {
                1
            } else {
                0
            };
        let operand = &after[operator..];
        let mut depth = 0_usize;
        let end = operand.char_indices().find_map(|(index, c)| match c {
            '{' => {
                depth += 1;
                None
            }
            '}' if depth == 0 => Some(index),
            '}' => {
                depth -= 1;
                None
            }
            _ => None,
        });
        match end {
            Some(end) if name > 0 && operator > 0 => {
                output.push_str(&operand[..end]);
                changed = true;
                rest = &operand[end + 1..];
            }
            _ => {
                output.push_str("${");
                rest = body;
            }
        }
    }
    output.push_str(rest);
    changed.then_some(output)
}

/// A command word's program name: its last path segment, lowercased,
/// without `.exe`.
fn command_name(word: &str) -> String {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let name = name.to_ascii_lowercase();
    if let Some(stem) = name.strip_suffix(".exe") {
        return stem.to_owned();
    }
    name
}

/// Whether `words[index]` is a script a shell runs: the argument after
/// `sh -c` (any flag cluster with `c`, as in `bash -lc`, past other options
/// and their values, as in `bash -o pipefail -c`), after `su -c` or
/// `script -c`, which hand it to a shell, or after `eval`.
fn is_shell_script<S: AsRef<str>>(words: &[S], index: usize) -> bool {
    let mut script_flag = false;
    for previous in (0..index).rev() {
        let word = words[previous].as_ref();
        if let Some(flag) = word.strip_prefix('-') {
            script_flag |= !flag.starts_with('-') && flag.contains('c');
            continue;
        }
        // `+o pipefail`, `+x`.
        if word.len() > 1 && word.starts_with('+') {
            continue;
        }
        if previous > 0 && takes_shell_value(words[previous - 1].as_ref()) {
            continue;
        }
        let name = command_name(word);
        return (script_flag
            && matches!(
                name.as_str(),
                "sh" | "bash"
                    | "zsh"
                    | "dash"
                    | "ksh"
                    | "mksh"
                    | "ash"
                    | "yash"
                    | "fish"
                    | "su"
                    | "script"
            ))
            || (previous + 1 == index && name == "eval");
    }
    false
}

/// Whether a shell option consumes the next word as its value: `-o`/`+o`
/// and `-O`/`+O` (also ending a flag cluster, as in `-eo`), `--rcfile`,
/// `--init-file`, and fish's `-C`/`--init-command`.
fn takes_shell_value(flag: &str) -> bool {
    matches!(flag, "--rcfile" | "--init-file" | "--init-command" | "-C")
        || (flag.len() > 1
            && (flag.starts_with('+') || (flag.starts_with('-') && !flag.starts_with("--")))
            && flag.ends_with(['o', 'O']))
}

/// Whether the words run inline interpreter code (`python3 -c`, `node -e`,
/// `ruby -e`, `perl -e`, `php -r`, `lua -e`, `Rscript -e`, `osascript -e`,
/// `deno eval`). Code builds paths at run
/// time (`'docs/' + name`), so no lexical match can rule an ignored path out.
fn runs_inline_code<S: AsRef<str>>(words: &[S]) -> bool {
    (0..words.len()).any(|index| {
        let name = command_name(words[index].as_ref());
        let versioned = |prefix: &str| {
            name.strip_prefix(prefix)
                .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit() || c == '.'))
        };
        // The inline-code flag letters, and the options that take the
        // next word as their value (`python3 -W ignore -c …`).
        let (letters, values): (&[char], &[&str]) =
            if versioned("python") || versioned("pypy") || name == "py" {
                (&['c'], &["-W", "-X", "--check-hash-based-pycs"])
            } else if versioned("perl") {
                (&['e', 'E'], &["-I", "-M", "-m"])
            } else if versioned("ruby") {
                (&['e'], &["-I", "-r", "-C", "-E"])
            } else if matches!(name.as_str(), "node" | "nodejs" | "bun") {
                (
                    &['e', 'p'],
                    &[
                        "-r",
                        "--require",
                        "--import",
                        "--loader",
                        "--experimental-loader",
                        "-C",
                        "--conditions",
                        "--input-type",
                        "--title",
                        "--env-file",
                    ],
                )
            } else if versioned("php") {
                (&['r'], &["-d", "-c", "-z"])
            } else if matches!(name.as_str(), "lua" | "luajit" | "rscript" | "osascript") {
                (&['e'], &["-l"])
            } else if name == "deno" {
                return words
                    .get(index + 1)
                    .is_some_and(|next| next.as_ref() == "eval");
            } else {
                return false;
            };
        let mut rest = words[index + 1..].iter().map(AsRef::as_ref);
        while let Some(flag) = rest.next() {
            if !flag.starts_with('-') {
                return false;
            }
            if matches!(flag, "--eval" | "--print")
                || flag.starts_with("--eval=")
                || flag.starts_with("--print=")
                || (!flag.starts_with("--") && flag[1..].contains(letters))
            {
                return true;
            }
            if values.contains(&flag) {
                rest.next();
            }
        }
        false
    })
}

/// Whether an unquoted word of `command` holds syntax the lexical matcher
/// does not evaluate: a brace expansion (`{a,b}`, `{1..3}`), a bracket class
/// (`[ab]`, `[]a]`), a zsh glob group or extglob (`docs/(a|b)`, `(a|b)/x`,
/// `@(x)`), or an ANSI-C string with a numeric or control escape
/// (`$'\x64'`). Quoted or escaped braces and brackets are literal, `${…}` is
/// a parameter expansion, `$(…)`, `<(…)` and a standalone `( … )` subshell
/// are not globs, and `{}` or a lone `[` (`find -exec … {}`, `[ -f x ]`)
/// expands nothing.
fn has_unevaluated_expansion(command: &str) -> bool {
    let is_operator = |c: char| matches!(c, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`');
    let is_word = |c: Option<&char>| c.is_some_and(|c| !c.is_whitespace() && !is_operator(*c));
    let mut quote = None;
    let mut previous = None;
    let mut in_word = false;
    let mut parameter = false;
    let mut brace = None::<bool>;
    let mut class = None::<usize>;
    // One entry per open unquoted `(`: whether it starts a word, where zsh
    // reads a following glob (`(a|b)/x`) instead of a subshell.
    let mut groups = Vec::new();
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            match (open, c) {
                ('$' | '\'', '\'') | ('"', '"') => quote = None,
                // The guard consumes the escaped character either way.
                ('$', '\\')
                    if chars.next().is_some_and(|escape| {
                        matches!(escape, 'x' | 'u' | 'U' | 'c' | '0'..='7')
                    }) =>
                {
                    return true;
                }
                ('"', '\\') => {
                    chars.next();
                }
                _ => {}
            }
            class = class.map(|len| len + 1);
            previous = None;
            in_word = true;
            continue;
        }
        match c {
            // `$'…'` is an ANSI-C string, tracked as its own quote.
            '\'' if previous == Some('$') => quote = Some('$'),
            '\'' | '"' => quote = Some(c),
            '\\' => {
                chars.next();
                class = class.map(|len| len + 1);
                previous = None;
                in_word = true;
                continue;
            }
            '(' => {
                if in_word && !matches!(previous, Some('$' | '=')) {
                    return true;
                }
                groups.push(!in_word && !matches!(previous, Some('<' | '>')));
                (parameter, brace, class, in_word) = (false, None, None, false);
            }
            ')' => {
                if groups.pop() == Some(true) && is_word(chars.peek()) {
                    return true;
                }
                (parameter, brace, class, in_word) = (false, None, None, false);
            }
            '|' if groups.last() == Some(&true) && in_word && is_word(chars.peek()) => {
                return true;
            }
            c if c.is_whitespace() || is_operator(c) => {
                (parameter, brace, class, in_word) = (false, None, None, false);
            }
            '{' if previous == Some('$') => parameter = true,
            '}' if parameter => parameter = false,
            _ if parameter => {}
            '{' => brace = Some(false),
            ',' if brace.is_some() => brace = Some(true),
            '.' if brace.is_some() && previous == Some('.') => brace = Some(true),
            '}' if brace.is_some() => {
                if brace == Some(true) {
                    return true;
                }
                brace = None;
            }
            '[' if class.is_none() => class = Some(0),
            // A `]` first in a class is a member (`[]a]`), not its end.
            ']' if class == Some(0) => class = Some(1),
            ']' if class.is_some() => return true,
            _ => class = class.map(|len| len + 1),
        }
        if !c.is_whitespace() && !is_operator(c) {
            in_word = true;
        }
        previous = Some(c);
    }
    false
}

fn shell_command(args: &Value) -> Option<ShellCommand<'_>> {
    match args
        .get("command")
        .or_else(|| args.get("cmd"))
        .or_else(|| args.get("CommandLine"))?
    {
        Value::String(command) => Some(ShellCommand::Line(command)),
        Value::Array(items) => items
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
            .map(ShellCommand::Argv),
        _ => None,
    }
}

/// Splits a command line into words the way a POSIX shell would before
/// expansion: quotes group, and whitespace and control/redirection operators
/// separate. A backslash escapes only a character the shell treats specially,
/// so Windows paths (`docs\adr\x.md`) keep their separators. A backquote
/// opens or closes a command substitution, so it separates too. An ANSI-C
/// string (`$'…'`) has its escapes decoded; numeric and control escapes stay
/// as written, and [`has_unevaluated_expansion`] reports them.
fn shell_words(command: &str) -> Vec<String> {
    let is_operator = |c: char| matches!(c, '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`');
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut quote = None;
    let mut chars = command.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some('\'' | '$'), '\'') | (Some('"'), '"') => quote = None,
            // A backslash-newline outside single quotes continues the line.
            (Some('"') | None, '\\') if chars.peek() == Some(&'\n') => {
                chars.next();
            }
            (Some('"'), '\\') if matches!(chars.peek(), Some('"' | '\\')) => {
                word.extend(chars.next());
            }
            (Some('$'), '\\') => match chars.next() {
                Some(escape) => match ansi_c_escape(escape) {
                    Some(decoded) => word.push(decoded),
                    None => {
                        word.push('\\');
                        word.push(escape);
                    }
                },
                None => word.push('\\'),
            },
            (Some(_), _) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            // `$'…'` (ANSI-C) and `$"…"` (locale) strings drop the `$`.
            (None, '$') if matches!(chars.peek(), Some('\'' | '"')) => {
                quote = match chars.next() {
                    Some('\'') => Some('$'),
                    _ => Some('"'),
                };
                in_word = true;
            }
            (None, '\\')
                if chars.peek().is_some_and(|next| {
                    next.is_whitespace() || is_operator(*next) || matches!(next, '\'' | '"' | '\\')
                }) =>
            {
                word.extend(chars.next());
                in_word = true;
            }
            (None, c) if c.is_whitespace() || is_operator(c) => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// The character an ANSI-C escape stands for, or `None` for a numeric or
/// control escape (`\x64`, `\144`, `\u0064`, `\cA`) that is not decoded.
fn ansi_c_escape(escape: char) -> Option<char> {
    Some(match escape {
        'a' => '\u{7}',
        'b' => '\u{8}',
        'e' | 'E' => '\u{1b}',
        'f' => '\u{c}',
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        'v' => '\u{b}',
        '\\' | '\'' | '"' | '?' => escape,
        _ => return None,
    })
}

/// Path-like values carried by one shell word: the word itself unless it is
/// a flag, plus the value of a `--flag=value`, `NAME=value`, or PowerShell
/// `-Param:value` word, plus a value glued to a short flag (`-fFILE`), plus
/// the path of a git `REV:path` object name, plus the path of any of these
/// that is a `file:` URL. The flag is set for the object name's path, which
/// git resolves from the repository root.
fn shell_arguments(word: &str) -> Vec<(String, bool)> {
    let flag = word.starts_with('-');
    let mut arguments: Vec<(String, bool)> = Vec::new();
    if !flag {
        arguments.push((word.to_owned(), false));
    }
    if let Some((_, value)) = word
        .split_once('=')
        .or_else(|| flag.then(|| word.split_once(':')).flatten())
    {
        arguments.push((value.to_owned(), false));
    }
    if let Some(cluster) = word.strip_prefix('-').filter(|rest| !rest.starts_with('-')) {
        let mut chars = cluster.chars();
        chars.next();
        if !chars.as_str().is_empty() {
            arguments.push((chars.as_str().to_owned(), false));
        }
    }
    if let Some(path) = git_object_path(word) {
        arguments.push((path.to_owned(), true));
    }
    let urls: Vec<_> = arguments
        .iter()
        .filter_map(|(argument, _)| file_url_path(argument))
        .map(|path| (path, false))
        .collect();
    arguments.extend(urls);
    arguments.retain(|(argument, _)| !argument.trim().is_empty());
    arguments
}

/// The local path a `file:` URL names (`file:///x`, `file://host/x`,
/// `file:/x`, any case), with ASCII percent-escapes decoded.
fn file_url_path(argument: &str) -> Option<String> {
    if !argument.get(..5)?.eq_ignore_ascii_case("file:") {
        return None;
    }
    let rest = &argument[5..];
    let path = match rest.strip_prefix("//") {
        Some(authority) => &authority[authority.find('/')?..],
        None => rest,
    };
    // `file:///C:/x` names the drive path `C:/x`.
    let path = path
        .strip_prefix('/')
        .filter(|drive| valid_drive_prefix(drive))
        .unwrap_or(path);
    let mut decoded = String::with_capacity(path.len());
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        let escape = chars.as_str().get(..2).and_then(|hex| {
            u8::from_str_radix(hex, 16)
                .ok()
                .filter(|byte| byte.is_ascii() && hex.chars().all(|d| d.is_ascii_hexdigit()))
        });
        match escape {
            Some(byte) if c == '%' => {
                decoded.push(char::from(byte));
                chars.nth(1);
            }
            _ => decoded.push(c),
        }
    }
    Some(decoded)
}

/// The path of a git object name such as `HEAD:docs/x.md`, `v1.2~3:x`,
/// `:docs/x.md` (the index) or `:2:x` (a merge stage). A drive letter
/// (`C:\x`), a URL (`https://…`) and anything whose left side is not
/// revision-shaped (`host:path`, `NAME=a:b`) are not object names here.
fn git_object_path(word: &str) -> Option<&str> {
    if word.starts_with('-') {
        return None;
    }
    let (revision, path) = word.split_once(':')?;
    let path = if revision.is_empty() {
        path.strip_prefix(['0', '1', '2', '3'])
            .and_then(|rest| rest.strip_prefix(':'))
            .unwrap_or(path)
    } else {
        path
    };
    let drive = revision.len() == 1 && revision.as_bytes()[0].is_ascii_alphabetic();
    let revision_shaped = revision.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | '-' | '/' | '~' | '^' | '@' | '{' | '}')
    });
    (!drive && revision_shaped && !path.starts_with("//")).then_some(path)
}

#[derive(Clone)]
struct Normalized {
    path: String,
    flavor: Flavor,
}
fn normalize_candidate(candidate: &str, cwd: &str) -> Option<Normalized> {
    if candidate.trim().is_empty()
        || candidate.chars().count() > MAX_CANDIDATE_PATH_CHARS
        || is_drive_relative(candidate)
    {
        return None;
    }
    let cwd = normalize_root(cwd).ok()?;
    // Flavor must come from the host (the cwd), never from the candidate
    // string alone: on a POSIX host a leading `//` is an ordinary doubled
    // separator, not a UNC root, but `flavor_of` cannot tell the two apart
    // from the string in isolation. Collapsing it first keeps a POSIX
    // candidate POSIX-flavored so it still matches a POSIX `ignore_paths`
    // pattern instead of silently escaping every pattern via a flavor
    // mismatch (GHSA-vh98). A genuine Windows/UNC host is unaffected: the
    // collapse only runs when the cwd itself is not windows-flavored.
    let raw = if is_absolute(candidate) {
        candidate.to_owned()
    } else {
        join(&cwd, candidate)
    };
    let raw = if flavor_of(&cwd) == Flavor::Posix && raw.starts_with("//") {
        format!("/{}", raw.trim_start_matches('/'))
    } else {
        raw
    };
    let flavor = flavor_of(&raw);
    Some(Normalized {
        path: normalize_segments(&raw)?,
        flavor,
    })
}
fn compile(
    config: &CaptureConfig,
    marker_dir: &str,
    home_dir: Option<&str>,
) -> Result<Vec<CompiledPattern>, String> {
    if config.ignore_paths.len() > MAX_IGNORE_PATTERNS {
        return Err(format!(
            "`ignore_paths` has more than {MAX_IGNORE_PATTERNS} entries"
        ));
    }
    let marker = normalize_root(marker_dir)
        .map_err(|_| "the repository path could not be normalized".to_owned())?;
    let needs_home = config
        .ignore_paths
        .iter()
        .any(|pattern| pattern.starts_with("~/"));
    let home = if needs_home {
        Some(
            home_dir
                .and_then(|dir| normalize_root(dir).ok())
                .ok_or_else(|| {
                    "a pattern starts with '~/' but no home directory is available to expand it"
                        .to_owned()
                })?,
        )
    } else {
        None
    };
    config
        .ignore_paths
        .iter()
        .map(|source| {
            validate_glob(source)
                .map_err(|reason| format!("ignore_paths entry {source:?} {reason}"))?;
            let expanded = if let Some(rest) = source.strip_prefix("~/") {
                let home = home.as_deref().ok_or_else(|| {
                    "a pattern starts with '~/' but no home directory is available to expand it"
                        .to_owned()
                })?;
                join(home, rest)
            } else if is_absolute(source) {
                source.clone()
            } else {
                join(&marker, source)
            };
            let flavor = flavor_of(&expanded);
            let path = normalize_segments(&expanded).ok_or_else(|| {
                format!("ignore_paths entry {source:?} could not be normalized into a path")
            })?;
            Ok(CompiledPattern {
                literal_prefix: literal_prefix(&path).into(),
                directory_base: path.strip_suffix("/**").map(|base| {
                    if base.is_empty() {
                        "/".into()
                    } else {
                        base.into()
                    }
                }),
                path,
                flavor,
            })
        })
        .collect()
}

/// Diagnostic twin of the compile step [`CapturePolicy::resolve`] runs: a
/// `[capture]` table can parse as valid TOML and still fail here (too many
/// patterns, an unsupported glob character, a `..` segment, a `~/` pattern
/// with no home directory to expand it...), which is exactly the gap a
/// parse-only check (e.g. `read_capture_config_verbose`) misses. `None`
/// means this config would compile into an active policy; `Some(reason)`
/// names the first rejected pattern and why.
#[must_use]
pub fn describe_invalid_capture_config(
    config: &CaptureConfig,
    marker_dir: &str,
    home_dir: Option<&str>,
) -> Option<String> {
    if config.ignore_paths.is_empty() {
        return None;
    }
    compile(config, marker_dir, home_dir).err()
}
fn validate_glob(pattern: &str) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err("is empty".to_owned());
    }
    if pattern.chars().count() > MAX_IGNORE_PATTERN_CHARS {
        return Err(format!(
            "is longer than {MAX_IGNORE_PATTERN_CHARS} characters"
        ));
    }
    if pattern.contains(['!', '{', '}', '[', ']', '(', ')', '|', '^', '$', '%']) {
        return Err(
            "contains an unsupported glob character (one of ! { } [ ] ( ) | ^ $ %)".to_owned(),
        );
    }
    if pattern.contains("${") {
        return Err("contains a shell-style variable expansion (${...})".to_owned());
    }
    if pattern.contains("***") {
        return Err("contains three or more consecutive '*'".to_owned());
    }
    if pattern
        .replace('\\', "/")
        .split('/')
        .any(|segment| segment == "..")
    {
        return Err("contains a '..' path segment".to_owned());
    }
    if pattern.starts_with('~') && !pattern.starts_with("~/") {
        return Err(
            "starts with '~' but not '~/' (only home-relative '~/...' is supported)".to_owned(),
        );
    }
    if is_drive_relative(pattern) {
        return Err("is drive-relative (e.g. 'C:foo' without a path separator)".to_owned());
    }
    Ok(())
}
fn join(base: &str, child: &str) -> String {
    format!("{}/{}", base.trim_end_matches(['/', '\\']), child)
}
fn flavor_of(path: &str) -> Flavor {
    if path.starts_with("\\\\") || path.starts_with("//") || valid_drive_prefix(path) {
        Flavor::Windows
    } else {
        Flavor::Posix
    }
}
fn valid_drive_prefix(path: &str) -> bool {
    path.len() >= 2 && path.as_bytes()[0].is_ascii_alphabetic() && path.as_bytes()[1] == b':'
}
fn is_absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || (valid_drive_prefix(path)
            && path.len() >= 3
            && matches!(path.as_bytes()[2], b'/' | b'\\'))
}
fn is_drive_relative(path: &str) -> bool {
    path.len() >= 2
        && path.as_bytes()[1] == b':'
        && (!path.as_bytes()[0].is_ascii_alphabetic() || !is_absolute(path))
}
fn normalize_root(path: &str) -> Result<String, ()> {
    (is_absolute(path) && !is_drive_relative(path))
        .then(|| normalize_segments(path))
        .flatten()
        .ok_or(())
}
fn normalize_segments(path: &str) -> Option<String> {
    let path = path.replace('\\', "/");
    let (root, tail): (String, Vec<&str>) = if let Some(rest) = path.strip_prefix("//") {
        let mut parts = rest.split('/').filter(|p| !p.is_empty());
        let server = parts.next()?;
        let share = parts.next()?;
        (format!("//{server}/{share}"), parts.collect())
    } else if valid_drive_prefix(&path) && path.as_bytes().get(2) == Some(&b'/') {
        (
            format!("{}:/", path[..1].to_ascii_uppercase()),
            path[3..].split('/').collect(),
        )
    } else {
        let rest = path.strip_prefix('/')?;
        ("/".into(), rest.split('/').collect())
    };
    let mut parts = Vec::new();
    for part in tail {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            parts.pop();
        } else {
            parts.push(part);
        }
    }
    let mut output = root;
    if !parts.is_empty() {
        if !output.ends_with('/') {
            output.push('/');
        }
        output.push_str(&parts.join("/"));
    }
    Some(output)
}
fn glob_match(
    pattern: &str,
    candidate: &str,
    directory_base: Option<&str>,
    insensitive: bool,
) -> bool {
    // `dir/**` also names `dir` itself, and `dir` may hold globs
    // (`docs/a?r/**`), exactly as in the generated TypeScript.
    if directory_base.is_some_and(|base| glob_match(base, candidate, None, insensitive)) {
        return true;
    }
    glob_states(pattern, candidate, insensitive)
        .last()
        .copied()
        .unwrap_or(false)
}
/// Whether `pattern` matches some path strictly below `directory`. A pattern
/// prefix still alive after `directory/` can always be completed by spelling
/// out its remaining literals.
fn glob_matches_below(pattern: &str, directory: &str, insensitive: bool) -> bool {
    let directory = if directory.ends_with('/') {
        directory.to_owned()
    } else {
        format!("{directory}/")
    };
    glob_states(pattern, &directory, insensitive)
        .iter()
        .any(|alive| *alive)
}
/// For each prefix length of `pattern`, whether that prefix matches all of
/// `candidate`.
fn glob_states(pattern: &str, candidate: &str, insensitive: bool) -> Vec<bool> {
    let pattern: Vec<char> = pattern.chars().collect();
    let candidate: Vec<char> = candidate.chars().collect();
    let mut previous = vec![false; pattern.len() + 1];
    previous[0] = true;
    for index in 1..=pattern.len() {
        previous[index] = match pattern[index - 1] {
            '*' if pattern.get(index) == Some(&'*') => false,
            '*' => previous[index - 1],
            _ => false,
        };
    }
    for character in candidate {
        let mut current = vec![false; pattern.len() + 1];
        for index in 1..=pattern.len() {
            current[index] = match pattern[index - 1] {
                '*' if pattern.get(index) == Some(&'*') => false,
                '*' if index >= 2 && pattern[index - 2] == '*' => {
                    current[index - 2] || previous[index]
                }
                '*' => current[index - 1] || (character != '/' && previous[index]),
                '?' => character != '/' && previous[index - 1],
                expected => char_equal(expected, character, insensitive) && previous[index - 1],
            };
        }
        previous = current;
    }
    previous
}
/// The whole leading segments of a normalized pattern that contain no glob.
fn literal_prefix(path: &str) -> &str {
    let Some(glob) = path.find(['*', '?']) else {
        return path;
    };
    let Some(slash) = path[..glob].rfind('/') else {
        return "";
    };
    let head = &path[..=slash];
    // Keep the root separator of `/`, `C:/`; drop it everywhere else.
    match head.strip_suffix('/') {
        Some(trimmed) if !trimmed.is_empty() && !trimmed.ends_with(':') => trimmed,
        _ => head,
    }
}
fn starts_with_chars(path: &str, prefix: &str, insensitive: bool) -> bool {
    let mut path = path.chars();
    prefix.chars().all(|expected| {
        path.next()
            .is_some_and(|c| char_equal(expected, c, insensitive))
    })
}
/// Whether a shell glob can expand to `prefix` itself or to a path below it.
/// Only the glob's first segments, as many as `prefix` has, are compared:
/// `docs/*/x.md` reaches `docs/adr`, while `*.md` in the parent never can.
fn glob_reaches(glob: &str, prefix: &str, insensitive: bool) -> bool {
    let prefix = prefix.trim_end_matches('/');
    let depth = prefix.split('/').filter(|part| !part.is_empty()).count();
    if depth == 0 {
        return true;
    }
    let mut seen = 0;
    let mut offset = 0;
    for part in glob.split('/') {
        offset += part.len();
        if !part.is_empty() {
            seen += 1;
            if seen == depth {
                return glob_match(&glob[..offset], prefix, None, insensitive);
            }
        }
        offset += 1;
    }
    false
}
/// `glob_reaches` for a glob whose `**` spans any number of segments, as in
/// Gemini CLI's `read_many_files`: once the segments before the first `**`
/// lead into `prefix` (or `prefix` lies below them), the glob can reach it.
fn globstar_reaches(glob: &str, prefix: &str, insensitive: bool) -> bool {
    let Some(star) = glob.find("**") else {
        return glob_reaches(glob, prefix, insensitive);
    };
    let head = glob[..star].rfind('/').map_or("", |slash| &glob[..slash]);
    let depth = head.split('/').filter(|part| !part.is_empty()).count();
    let mut seen = 0;
    let mut offset = 0;
    for part in prefix.split('/') {
        if seen == depth {
            break;
        }
        offset += part.len();
        if !part.is_empty() {
            seen += 1;
        }
        if seen < depth {
            offset += 1;
        }
    }
    if seen < depth {
        // `prefix` is shallower than the literal head; compare as usual.
        return glob_reaches(glob, prefix, insensitive);
    }
    glob_match(head, &prefix[..offset.min(prefix.len())], None, insensitive)
}
fn char_equal(left: char, right: char, insensitive: bool) -> bool {
    if insensitive && left.is_ascii() && right.is_ascii() {
        left.eq_ignore_ascii_case(&right)
    } else {
        left == right
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The routed `cwd` is built from these paths, so a Windows target keeps
    /// backslashes (as the harness reports every other cwd) while `..` is
    /// still collapsed; a POSIX target is unchanged. Windows CI caught the
    /// forward-slash `C:/…` form being sent as the destination cwd.
    #[test]
    fn absolute_tool_location_paths_keep_the_native_separator_style() {
        let edit = |path: &str| {
            serde_json::json!({
                "tool_name": "Edit",
                "tool_input": {"file_path": path}
            })
        };
        assert_eq!(
            absolute_tool_location_paths(
                AgentKind::ClaudeCode,
                &edit(r"C:\work\destination\sub\..\a.txt"),
                r"C:\work\source",
            ),
            Some(vec![r"C:\work\destination\a.txt".to_owned()])
        );
        assert_eq!(
            absolute_tool_location_paths(
                AgentKind::ClaudeCode,
                &edit("/work/destination/sub/../a.txt"),
                "/work/source",
            ),
            Some(vec!["/work/destination/a.txt".to_owned()])
        );
    }

    /// A tool's location is its file targets, a search tool's explicit
    /// directory, or a shell command's structured `workdir` — absolute only,
    /// and never a path parsed out of the command text (#1199).
    #[test]
    fn absolute_tool_location_paths_cover_search_dirs_and_shell_workdirs() {
        let hermes = |tool: &str, input: Value| {
            absolute_tool_location_paths(
                AgentKind::Hermes,
                &serde_json::json!({"tool_name": tool, "tool_input": input}),
                "/home/user",
            )
        };
        assert_eq!(
            hermes(
                "terminal",
                serde_json::json!({"command": "ls", "workdir": "/work/repo/./src/.."})
            ),
            Some(vec!["/work/repo".to_owned()])
        );
        assert_eq!(
            hermes(
                "search_files",
                serde_json::json!({"pattern": "x", "path": "/work/repo"})
            ),
            Some(vec!["/work/repo".to_owned()])
        );
        assert_eq!(
            hermes("read_file", serde_json::json!({"path": "/work/repo/a.rs"})),
            Some(vec!["/work/repo/a.rs".to_owned()])
        );
        for (tool, input) in [
            (
                "terminal",
                serde_json::json!({"command": "ls", "workdir": "repo"}),
            ),
            (
                "terminal",
                serde_json::json!({"command": "cat /work/repo/a.rs"}),
            ),
            ("terminal", serde_json::json!({"workdir": "/work/repo"})),
            (
                "search_files",
                serde_json::json!({"pattern": "x", "path": "."}),
            ),
            ("search_files", serde_json::json!({"pattern": "x"})),
            (
                "process",
                serde_json::json!({"action": "list", "workdir": "/work/repo"}),
            ),
        ] {
            assert_eq!(hermes(tool, input.clone()), None, "{tool} {input}");
        }
    }

    #[test]
    fn denylist_admits_every_repository() {
        assert!(repository_admits_capture(CaptureMode::Denylist, false));
        assert!(repository_admits_capture(CaptureMode::Denylist, true));
    }

    #[test]
    fn allowlist_admits_only_a_marked_repository() {
        assert!(!repository_admits_capture(CaptureMode::Allowlist, false));
        assert!(repository_admits_capture(CaptureMode::Allowlist, true));
    }

    #[test]
    fn default_mode_is_the_historical_one() {
        // A new field must not silently tighten capture for existing installs.
        assert_eq!(CaptureMode::default(), CaptureMode::Denylist);
    }

    #[test]
    fn state_reports_active_for_a_valid_config() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        assert_eq!(policy.state(), PolicyState::Active);
    }

    #[test]
    fn state_reports_inactive_for_an_absent_marker() {
        let policy = CapturePolicy::resolve(CaptureSource::Absent, "/repo", None);
        assert_eq!(policy.state(), PolicyState::Inactive);
    }

    #[test]
    fn state_reports_inactive_for_an_empty_ignore_paths() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: Vec::new(),
            }),
            "/repo",
            None,
        );
        assert_eq!(policy.state(), PolicyState::Inactive);
    }

    #[test]
    fn state_reports_invalid_for_a_parse_failure() {
        let policy = CapturePolicy::resolve(CaptureSource::Invalid, "/repo", None);
        assert_eq!(policy.state(), PolicyState::Invalid);
    }

    #[test]
    fn state_reports_invalid_for_a_config_that_parses_but_does_not_compile() {
        // This is exactly the gap a parse-only check misses: valid TOML,
        // valid `[capture]` shape, but a pattern `compile` itself rejects.
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secrets/{a,b}".into()],
            }),
            "/repo",
            None,
        );
        assert_eq!(policy.state(), PolicyState::Invalid);
    }

    #[test]
    fn describe_invalid_capture_config_is_none_when_it_would_compile() {
        let config = CaptureConfig {
            ignore_paths: vec!["secret/**".into()],
        };
        assert!(describe_invalid_capture_config(&config, "/repo", None).is_none());
    }

    #[test]
    fn describe_invalid_capture_config_names_the_rejected_pattern() {
        let config = CaptureConfig {
            ignore_paths: vec!["ok/**".into(), "secrets/{a,b}".into()],
        };
        let reason = describe_invalid_capture_config(&config, "/repo", None)
            .expect("a rejected glob must produce a reason");
        assert!(reason.contains("secrets/{a,b}"), "{reason}");
        assert!(reason.contains("unsupported glob character"), "{reason}");
    }

    #[test]
    fn describe_invalid_capture_config_is_none_for_an_empty_config() {
        let config = CaptureConfig {
            ignore_paths: Vec::new(),
        };
        assert!(describe_invalid_capture_config(&config, "/repo", None).is_none());
    }
    #[test]
    fn fixture_vectors() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/capture-policy.json")).unwrap();
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        for vector in fixture["decisions"].as_array().unwrap() {
            let agent = AgentKind::from_wire(vector["agent"].as_str().unwrap());
            let decision = policy.inspect(agent, &vector["payload"], "/repo");
            let protocol = decision.protocol();
            assert_eq!(
                serde_json::to_value(protocol.disposition()).unwrap(),
                vector["disposition"]
            );
            assert_eq!(
                serde_json::to_value(protocol.tool_family()).unwrap(),
                vector["tool_family"]
            );
            assert_eq!(
                serde_json::to_value(protocol.extraction_state()).unwrap(),
                vector["extraction_state"]
            );
            assert_eq!(
                protocol.path_count(),
                vector["path_count"].as_u64().unwrap() as u16
            );
        }
        for vector in fixture["normalization"].as_array().unwrap() {
            let policy = CapturePolicy::resolve(
                CaptureSource::Parsed(&CaptureConfig {
                    ignore_paths: vec![vector["pattern"].as_str().unwrap().into()],
                }),
                "/repo",
                None,
            );
            assert_eq!(
                policy
                    .match_paths(
                        &[vector["candidate"].as_str().unwrap().into()],
                        vector["cwd"].as_str().unwrap()
                    )
                    .unwrap(),
                vector["match"].as_bool().unwrap()
            );
        }
        assert!(CaptureProtocol::parse(&fixture["protocol"]["accept"]).is_some());
        assert!(CaptureProtocol::parse(&fixture["protocol"]["reject"]).is_none());
    }
    /// Adversarial regression for GHSA-vh98 / security-boundaries.md row
    /// 11b: a POSIX-host candidate spelled with a leading `//` used to be
    /// classified `Flavor::Windows` purely from the string, so it matched
    /// zero POSIX `ignore_paths` patterns (flavor mismatch in `match_paths`'
    /// filter) and was captured instead of dropped. `flavor_of` cannot tell
    /// a doubled POSIX separator from a UNC root by itself; only the host
    /// (the cwd) can. This attempts the violation, proves a plain-looking
    /// control still drops normally, and proves a genuine Windows-hosted UNC
    /// candidate still matches (so the fix didn't just blanket-collapse
    /// every `//`).
    #[test]
    fn a_leading_double_slash_candidate_does_not_escape_posix_ignore_paths_via_flavor_mismatch() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        // The violation attempt: on the unfixed code this normalized to a
        // Windows-flavored candidate and matched no POSIX pattern, so it
        // came back `Keep` (captured) instead of `Drop`.
        let attack =
            json!({"tool_name":"Edit","tool_input":{"file_path":"//repo/secret/token.txt"}});
        assert_eq!(
            policy
                .inspect(AgentKind::ClaudeCode, &attack, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Drop,
            "a leading `//` must not escape a POSIX host's ignore_paths"
        );
        // Legitimate control: an ordinary single-slash candidate under the
        // same pattern must keep being dropped.
        let control =
            json!({"tool_name":"Edit","tool_input":{"file_path":"/repo/secret/token.txt"}});
        assert_eq!(
            policy
                .inspect(AgentKind::ClaudeCode, &control, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Drop
        );
        // Windows-UNC control: a genuine UNC candidate on a Windows host
        // must still match a UNC pattern — the fix is host-derived, not an
        // unconditional `//` -> `/` collapse.
        let windows_policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["\\\\server\\share\\**".into()],
            }),
            "C:/",
            None,
        );
        let unc = json!({"tool_name":"Edit","tool_input":{"file_path":"//SERVER/SHARE/token.txt"}});
        assert_eq!(
            windows_policy
                .inspect(AgentKind::ClaudeCode, &unc, "C:/")
                .protocol()
                .disposition(),
            CaptureDisposition::Drop,
            "a genuine UNC candidate on a Windows host must still match"
        );
    }
    #[test]
    fn all_states_and_strict_protocol_are_reachable() {
        let file = json!({"tool_name":"Edit","tool_input":{}});
        assert_eq!(
            CapturePolicy::resolve(CaptureSource::Absent, "/repo", None)
                .inspect(AgentKind::Codex, &file, "/repo")
                .protocol()
                .policy_state(),
            PolicyState::Inactive
        );
        let invalid = CapturePolicy::resolve(CaptureSource::Invalid, "/repo", None).inspect(
            AgentKind::Codex,
            &file,
            "/repo",
        );
        assert_eq!(invalid.protocol().policy_state(), PolicyState::Invalid);
        assert_eq!(
            invalid.protocol().disposition(),
            CaptureDisposition::MetadataOnly
        );
        assert_eq!(
            CapturePolicy::resolve(
                CaptureSource::Parsed(&CaptureConfig {
                    ignore_paths: vec!["a/../b".into()]
                }),
                "/repo",
                None
            )
            .inspect(AgentKind::Codex, &file, "/repo")
            .protocol()
            .policy_state(),
            PolicyState::Invalid
        );
        assert_eq!(
            CapturePolicy::resolve(
                CaptureSource::Parsed(&CaptureConfig {
                    ignore_paths: vec!["x".into()]
                }),
                "/repo",
                None
            )
            .inspect(AgentKind::Other, &json!({}), "/repo")
            .protocol()
            .extraction_state(),
            ExtractionState::UnsupportedSchema
        );
        assert!(
            serde_json::from_value::<CaptureConfig>(json!({"ignore_paths": [], "mode": "keep"}))
                .is_err()
        );
        let value = serde_json::to_value(invalid.protocol()).unwrap();
        assert!(CaptureProtocol::parse(&value).is_some());
        let mut bad = value;
        bad.as_object_mut()
            .unwrap()
            .insert("paths".into(), json!(["x"]));
        assert!(CaptureProtocol::parse(&bad).is_none());
    }

    #[test]
    fn hermes_official_tool_shape_is_closed_and_honors_exclusions() {
        let raw = json!({
            "hook_event_name": "post_tool_call",
            "tool_name": "write_file",
            "tool_input": {"path": "secret/token.txt", "content": "do not retain"},
            "session_id": "hermes-session",
            "cwd": "/repo",
            "extra": {"tool_call_id": "call-42", "status": "ok"}
        });
        let metadata = tool_observation_metadata(AgentKind::Hermes, &raw, false).unwrap();
        assert_eq!(metadata.tool_family, ToolFamily::File);
        assert_eq!(metadata.tool_call_id.as_deref(), Some("call-42"));

        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        let decision = policy.inspect(AgentKind::Hermes, &raw, "/repo");
        assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Drop);

        let unknown = policy.inspect(AgentKind::Other, &raw, "/repo");
        assert_eq!(
            unknown.protocol().extraction_state(),
            ExtractionState::UnsupportedSchema
        );
        assert_eq!(unknown.protocol().tool_family(), ToolFamily::Unknown);
    }

    /// A transcript import rebuilds each recorded tool call with
    /// `tool_call_payload`; for every agent whose hook schema `extract`
    /// supports, that rebuilt call must reach the same decision a live hook
    /// gets, or `backfill` and the run ledger would store excluded content.
    #[test]
    fn tool_call_payload_reaches_the_live_decision_for_every_supported_agent() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            "/repo",
            None,
        );
        for agent in [
            AgentKind::AntigravityCli,
            AgentKind::Grok,
            AgentKind::Zero,
            AgentKind::ClaudeCode,
            AgentKind::CommandCode,
            AgentKind::Codex,
            AgentKind::Cursor,
            AgentKind::GeminiCli,
            AgentKind::Devin,
            AgentKind::Hermes,
            AgentKind::KiroCli,
            AgentKind::Pool,
            AgentKind::Grizzybot,
            AgentKind::Zcode,
            AgentKind::CopilotCli,
            AgentKind::OpenCode,
            AgentKind::Omp,
            AgentKind::Pi,
            AgentKind::OpenClaw,
        ] {
            let decide = |name: &str, args: Option<&Value>| {
                let raw = tool_call_payload(agent, name, args, Some("call-1"));
                policy.inspect(agent, &raw, "/repo")
            };
            let excluded = decide("read", Some(&json!({"file_path": "docs/adr/1.md"})));
            assert_eq!(
                excluded.protocol().disposition(),
                CaptureDisposition::Drop,
                "{agent:?}: excluded read"
            );
            assert_eq!(excluded.call_id.as_deref(), Some("call-1"), "{agent:?}");
            let public = decide("read", Some(&json!({"file_path": "src/lib.rs"})));
            assert_eq!(
                public.protocol().disposition(),
                CaptureDisposition::Keep,
                "{agent:?}: public read"
            );
            assert_eq!(public.protocol().tool_family(), ToolFamily::File);
            let shell = decide("bash", Some(&json!({"command": "cat docs/adr/1.md"})));
            assert_eq!(
                shell.protocol().disposition(),
                CaptureDisposition::Drop,
                "{agent:?}: shell naming an excluded path"
            );
            assert_eq!(
                decide("read", None).protocol().disposition(),
                CaptureDisposition::MetadataOnly,
                "{agent:?}: unrecoverable file arguments fail closed"
            );
        }
    }

    /// Codex transcripts record the shell tool as `exec_command` with
    /// `{cmd, workdir}`; hooks receive it as `Bash`. The rebuilt call must be a
    /// shell call resolved from its `workdir`, not an unknown tool whose output
    /// is kept.
    #[test]
    fn codex_transcript_exec_command_is_inspected_as_the_hook_bash_call() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            "/repo",
            None,
        );
        let decide = |agent: AgentKind, args: Value| {
            let raw = tool_call_payload(agent, "exec_command", Some(&args), Some("call-1"));
            policy.inspect(agent, &raw, "/repo").protocol().clone()
        };
        for args in [
            json!({"cmd": "cat adr/x.md", "workdir": "/repo/docs"}),
            json!({"cmd": "cat adr/x.md", "workdir": "docs"}),
            json!({"cmd": "cat docs/adr/x.md"}),
            json!({"cmd": ["bash", "-lc", "cat docs/adr/x.md"], "yield_time_ms": 1000}),
        ] {
            let protocol = decide(AgentKind::Codex, args.clone());
            assert_eq!(protocol.tool_family(), ToolFamily::NonFile, "{args}");
            assert_eq!(protocol.disposition(), CaptureDisposition::Drop, "{args}");
        }
        let public = decide(
            AgentKind::Codex,
            json!({"cmd": "cat adr/x.md", "workdir": "/repo/src"}),
        );
        assert_eq!(public.tool_family(), ToolFamily::NonFile);
        assert_eq!(public.disposition(), CaptureDisposition::Keep);
        let raw = tool_call_payload(
            AgentKind::Codex,
            "exec_command",
            Some(&json!({"cmd": "ls", "workdir": "/other/repo"})),
            None,
        );
        assert_eq!(
            absolute_tool_location_paths(AgentKind::Codex, &raw, "/repo"),
            Some(vec!["/other/repo".to_owned()]),
            "the workdir locates the call like a live shell workdir"
        );
        // Only Codex records its shell under this name.
        assert_eq!(
            decide(AgentKind::ClaudeCode, json!({"cmd": "cat docs/adr/x.md"})).tool_family(),
            ToolFamily::Unknown
        );
    }

    #[test]
    fn hermes_documented_tool_names_map_to_canonical_families() {
        for (tool, expected) in [
            ("read_file", ToolFamily::File),
            ("write_file", ToolFamily::File),
            ("patch", ToolFamily::File),
            ("search_files", ToolFamily::SearchList),
            ("terminal", ToolFamily::NonFile),
        ] {
            assert_eq!(family(tool), expected, "tool: {tool}");
        }
    }

    #[test]
    fn pool_documented_tool_shape_is_closed_and_honors_exclusions() {
        let raw = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "write",
            "tool_input": {"path": "secret/token.txt", "content": "do not retain"},
            "session_id": "pool-session",
            "cwd": "/repo"
        });
        let metadata = tool_observation_metadata(AgentKind::Pool, &raw, true).unwrap();
        assert_eq!(metadata.tool_family, ToolFamily::File);
        assert_eq!(metadata.tool_call_id, None);

        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        let decision = policy.inspect(AgentKind::Pool, &raw, "/repo");
        assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Drop);

        let unknown = policy.inspect(AgentKind::Other, &raw, "/repo");
        assert_eq!(
            unknown.protocol().extraction_state(),
            ExtractionState::UnsupportedSchema
        );
        assert_eq!(unknown.protocol().tool_family(), ToolFamily::Unknown);
    }

    #[test]
    fn pool_documented_tool_names_map_to_canonical_families() {
        for (tool, expected) in [
            ("read", ToolFamily::File),
            ("edit", ToolFamily::File),
            ("write", ToolFamily::File),
            ("remove", ToolFamily::File),
            ("shell", ToolFamily::NonFile),
        ] {
            assert_eq!(family(tool), expected, "tool: {tool}");
        }
    }

    #[test]
    fn zcode_documented_tool_shape_is_closed_and_honors_exclusions() {
        // Live-captured ZCode payload (engine v0.16.5, #512): the snake_case
        // aliases carry exactly the fields Claude Code sends, including a
        // real provider-format tool call id (`call_…`).
        let raw = json!({
            "hookEventName": "PostToolUse",
            "toolName": "Write",
            "tool_name": "Write",
            "tool_use_id": "call_6d8f8fd5d9eb4888b0f9d5c6",
            "toolInput": {"file_path": "/repo/secret/token.txt", "content": "do not retain"},
            "tool_input": {"file_path": "/repo/secret/token.txt", "content": "do not retain"},
            "session_id": "sess_0a5ba797",
            "cwd": "/repo"
        });
        let metadata = tool_observation_metadata(AgentKind::Zcode, &raw, true).unwrap();
        assert_eq!(metadata.tool_family, ToolFamily::File);
        assert_eq!(
            metadata.tool_call_id.as_deref(),
            Some("call_6d8f8fd5d9eb4888b0f9d5c6")
        );

        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        let decision = policy.inspect(AgentKind::Zcode, &raw, "/repo");
        assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Drop);

        let unknown = policy.inspect(AgentKind::Other, &raw, "/repo");
        assert_eq!(
            unknown.protocol().extraction_state(),
            ExtractionState::UnsupportedSchema
        );
        assert_eq!(unknown.protocol().tool_family(), ToolFamily::Unknown);
    }

    #[test]
    fn grizzybot_tool_shape_is_closed_and_unknown_agents_stay_unsupported() {
        let raw = json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "web_search",
            "tool_input": {"query": "swift"},
            "session_id": "gb-session",
            "cwd": "/bot/home"
        });
        let metadata = tool_observation_metadata(AgentKind::Grizzybot, &raw, true).unwrap();
        assert_eq!(metadata.tool_family, ToolFamily::NonFile);
        // A PreToolUse without an input is not a proven shape.
        let no_input = json!({"tool_name": "read_file"});
        assert!(tool_observation_metadata(AgentKind::Grizzybot, &no_input, true).is_none());
        assert!(tool_observation_metadata(AgentKind::Grizzybot, &no_input, false).is_some());
    }

    #[test]
    fn zcode_post_tool_use_failure_error_maps_to_error_outcome() {
        // Live-captured PostToolUseFailure payload (run 10, real model, #512):
        // `error` is a plain string, `error_details` carries the internal
        // error class.
        let raw = json!({
            "hook_event_name": "PostToolUseFailure",
            "tool_name": "Write",
            "error": "File not found: /proc/capture-test.txt",
            "error_details": {
                "message": "File not found: /proc/capture-test.txt",
                "type": "FileSystemPortError"
            }
        });
        assert_eq!(
            tool_observation_outcome(AgentKind::Zcode, &raw),
            ToolOutcome::Error
        );
        // exitCode is deliberately not trusted for Success: the same
        // tool_response object carries timedOut / interrupted flags.
        let ok = json!({"tool_response": {"exitCode": 0}});
        assert_eq!(
            tool_observation_outcome(AgentKind::Zcode, &ok),
            ToolOutcome::Unknown
        );
    }

    #[test]
    fn copilot_cli_outcome_reads_only_proven_result_types() {
        let outcome = |raw: Value| tool_observation_outcome(AgentKind::CopilotCli, &raw);
        let with_type = |result_type: &str| {
            json!({"tool_name": "bash", "tool_result": {
                "result_type": result_type, "text_result_for_llm": "x"
            }})
        };
        assert_eq!(outcome(with_type("success")), ToolOutcome::Success);
        assert_eq!(outcome(with_type("failure")), ToolOutcome::Error);
        assert_eq!(outcome(with_type("error")), ToolOutcome::Error);
        // A denied or rejected call is not a tool failure, and a future type
        // proves nothing.
        assert_eq!(outcome(with_type("denied")), ToolOutcome::Unknown);
        // PostToolUseFailure (aliased to PostToolUse) carries `error` and no
        // `tool_result`.
        assert_eq!(
            outcome(json!({"tool_name": "bash", "error": "permission denied"})),
            ToolOutcome::Error
        );
        assert_eq!(outcome(json!({"tool_name": "bash"})), ToolOutcome::Unknown);
    }

    #[test]
    fn copilot_cli_tool_metadata_uses_the_claude_compatible_keys() {
        let pre = json!({
            "tool_name": "edit",
            "tool_input": {"path": "src/lib.rs"},
        });
        assert!(tool_observation_metadata(AgentKind::CopilotCli, &pre, true).is_some());
        // PreToolUse without a proven input shape stays metadata-less.
        let no_input = json!({"tool_name": "edit"});
        assert!(tool_observation_metadata(AgentKind::CopilotCli, &no_input, true).is_none());
    }

    #[test]
    fn command_code_official_tool_shape_is_closed_and_honors_exclusions() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        let ignored = json!({
            "session_id": "command-session",
            "cwd": "/repo",
            "tool_use_id": "tool-42",
            "tool_name": "edit_file",
            "tool_input": {
                "file_path": "secret/token.txt",
                "old_value": "old",
                "new_value": "new"
            }
        });

        let metadata = tool_observation_metadata(AgentKind::CommandCode, &ignored, true).unwrap();
        assert_eq!(metadata.tool_family, ToolFamily::File);
        assert_eq!(metadata.tool_call_id.as_deref(), Some("tool-42"));
        let decision = policy.inspect(AgentKind::CommandCode, &ignored, "/repo");
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Drop);

        for (tool, expected) in [
            ("read_file", ToolFamily::File),
            ("write_file", ToolFamily::File),
            ("edit_file", ToolFamily::File),
            ("shell_command", ToolFamily::NonFile),
        ] {
            assert_eq!(family(tool), expected, "tool: {tool}");
        }
    }

    #[test]
    fn kiro_v3_documented_and_live_tool_shapes_honor_exclusions_and_fail_closed() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        let official_read = json!({
            "session_id": "kiro-session",
            "cwd": "/repo",
            "tool_name": "read",
            "tool_input": {
                "operations": [
                    {"mode": "Line", "path": "src/lib.rs"},
                    {"mode": "Line", "path": "secret/token.txt"}
                ]
            }
        });
        let decision = policy.inspect(AgentKind::KiroCli, &official_read, "/repo");
        assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
        assert_eq!(decision.protocol().path_count(), 2);
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Drop);

        let live_read = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "kiro-session",
            "cwd": "/repo",
            "tool_name": "read_file",
            "tool_input": {
                "path": "secret/token.txt",
                "offset": null,
                "limit": null
            }
        });
        let decision = policy.inspect(AgentKind::KiroCli, &live_read, "/repo");
        assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
        assert_eq!(decision.protocol().path_count(), 1);
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Drop);

        let mut official_post = official_read.clone();
        official_post.as_object_mut().unwrap().insert(
            "tool_response".to_string(),
            json!({"success": true, "result": ["sanitized"]}),
        );
        assert_eq!(
            tool_observation_outcome(AgentKind::KiroCli, &official_post),
            ToolOutcome::Success
        );

        let unknown_shape = json!({
            "session_id": "kiro-session",
            "cwd": "/repo",
            "tool_name": "read",
            "tool_input": {"files": ["secret/token.txt"]}
        });
        let decision = policy.inspect(AgentKind::KiroCli, &unknown_shape, "/repo");
        assert_eq!(
            decision.protocol().extraction_state(),
            ExtractionState::MissingOrMalformed
        );
        assert_eq!(
            decision.protocol().disposition(),
            CaptureDisposition::MetadataOnly
        );
    }

    #[test]
    fn antigravity_native_tools_are_no_longer_unknown() {
        for (tool, expected) in [
            ("view_file", ToolFamily::File),
            ("replace_file_content", ToolFamily::File),
            ("multi_replace_file_content", ToolFamily::File),
            ("write_to_file", ToolFamily::File),
            ("list_dir", ToolFamily::SearchList),
            ("grep_search", ToolFamily::SearchList),
        ] {
            assert_eq!(family(tool), expected, "tool: {tool}");
        }
    }
    #[test]
    fn antigravity_target_file_is_a_proven_path() {
        let target = json!({"TargetFile": "/repo/src/main.rs"});
        assert_eq!(
            direct_paths(target.as_object().unwrap()).unwrap(),
            vec!["/repo/src/main.rs".to_string()]
        );
    }
    #[test]
    fn antigravity_file_tools_honor_capture_exclusions() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        for tool in [
            "view_file",
            "write_to_file",
            "replace_file_content",
            "multi_replace_file_content",
        ] {
            let ignored = json!({
                "toolCall": {
                    "name": tool,
                    "args": {"TargetFile": "secret/keys.txt"}
                }
            });
            let decision = policy.inspect(AgentKind::AntigravityCli, &ignored, "/repo");
            assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
            assert_eq!(
                decision.protocol().disposition(),
                CaptureDisposition::Drop,
                "tool: {tool}"
            );
        }

        let kept =
            json!({"toolCall": {"name": "view_file", "args": {"TargetFile": "src/main.rs"}}});
        let decision = policy.inspect(AgentKind::AntigravityCli, &kept, "/repo");
        assert_eq!(decision.protocol().tool_family(), ToolFamily::File);
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Keep);
    }
    #[test]
    fn normalization_and_matcher_bounds() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec![
                    r"C:\\Secret\\**".into(),
                    r"\\server\\share\\x".into(),
                    "unicode/?.txt".into(),
                ],
            }),
            "/repo",
            None,
        );
        assert!(policy.match_paths(&[r"c:/SECRET".into()], "C:/").unwrap());
        assert!(
            policy
                .match_paths(&[r"\\server\\share\\x".into()], "C:/")
                .unwrap()
        );
        assert!(
            policy
                .match_paths(&["unicode/é.txt".into()], "/repo")
                .unwrap()
        );
        assert!(policy.match_paths(&["C:bad".into()], "C:/").is_err());
        let raw = json!({"tool_name":"Edit","tool_input":{"path":"x".repeat(MAX_CANDIDATE_PATH_CHARS + 1)}});
        assert_eq!(
            policy
                .inspect(AgentKind::Codex, &raw, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::MetadataOnly
        );
        let overflow = json!({"tool_name":"Edit","tool_input":{"paths":vec!["public"; MAX_CAPTURE_CANDIDATES + 1]}});
        assert_eq!(
            policy
                .inspect(AgentKind::Codex, &overflow, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::MetadataOnly
        );
        let costly = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["?".repeat(MAX_IGNORE_PATTERN_CHARS)],
            }),
            "/repo",
            None,
        );
        let costly_raw =
            json!({"tool_name":"Edit","tool_input":{"path":"x".repeat(MAX_CANDIDATE_PATH_CHARS)}});
        assert_eq!(
            costly
                .inspect(AgentKind::Codex, &costly_raw, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::MetadataOnly
        );
        let chars = "?".repeat(MAX_IGNORE_PATTERN_CHARS);
        assert_eq!(CapturePolicy::resolve(CaptureSource::Parsed(&CaptureConfig { ignore_paths: vec![chars] }), "/repo", None).inspect(AgentKind::Codex, &json!({"tool_name":"Edit","tool_input":{"path":"x".repeat(MAX_IGNORE_PATTERN_CHARS)}}), "/repo").protocol().policy_state(), PolicyState::Active);
        assert_eq!(
            CapturePolicy::resolve(
                CaptureSource::Parsed(&CaptureConfig {
                    ignore_paths: vec!["?".repeat(MAX_IGNORE_PATTERN_CHARS + 1)]
                }),
                "/repo",
                None
            )
            .inspect(
                AgentKind::Codex,
                &json!({"tool_name":"Edit","tool_input":{"path":"x"}}),
                "/repo"
            )
            .protocol()
            .policy_state(),
            PolicyState::Invalid
        );
    }
    fn shell_policy() -> CapturePolicy {
        CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into(), "~/notes/**".into()],
            }),
            "/repo",
            Some("/home/me"),
        )
    }
    fn bash(command: &str) -> Value {
        json!({"tool_name": "Bash", "tool_input": {"command": command}, "tool_use_id": "call-1"})
    }

    /// zsh, and bash with `globstar`, recurse on `**`, so a shell glob whose
    /// `**` can descend into an ignored directory drops the event; a `**`
    /// rooted elsewhere is still kept (control).
    #[test]
    fn recursive_shell_globs_cannot_reach_ignored_paths() {
        let policy = shell_policy();
        for command in ["cat **/*.md", "cat **/0001.md", "cat docs/**/0001.md"] {
            let decision = policy.inspect(AgentKind::ClaudeCode, &bash(command), "/repo");
            assert_eq!(
                decision.protocol().disposition(),
                CaptureDisposition::Drop,
                "{command}"
            );
        }
        let decision = policy.inspect(AgentKind::ClaudeCode, &bash("cat src/**/main.rs"), "/repo");
        assert_eq!(decision.protocol().disposition(), CaptureDisposition::Keep);
    }

    /// The drop/keep tables live in the shared fixture so the generated
    /// TypeScript matcher runs the very same vectors (#961).
    #[test]
    fn shell_fixture_vectors() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/capture-policy.json")).unwrap();
        let shell = &fixture["shell"];
        let ignore_paths = shell["ignore_paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pattern| pattern.as_str().unwrap().to_owned())
            .collect();
        let config = CaptureConfig { ignore_paths };
        // The host's case folding is pinned per vector, so the table means
        // the same on every CI OS.
        let policy = |fold_case| {
            CapturePolicy::resolve_for_host(
                CaptureSource::Parsed(&config),
                "/repo",
                Some("/home/me"),
                fold_case,
            )
        };
        for vector in shell["vectors"].as_array().unwrap() {
            let policy = policy(vector["fold_case"].as_bool().unwrap_or(false));
            let payload = vector
                .get("payload")
                .cloned()
                .unwrap_or_else(|| json!({"tool": "bash", "args": {"command": vector["command"]}}));
            let payload: Value =
                serde_json::from_str(&payload.to_string().replace("{root}", "/repo")).unwrap();
            let cwd = vector["cwd"]
                .as_str()
                .map_or_else(|| "/repo".to_owned(), |sub| format!("/repo/{sub}"));
            let decision = policy.inspect(AgentKind::OpenCode, &payload, &cwd);
            let protocol = decision.protocol();
            assert_eq!(
                serde_json::to_value(protocol.disposition()).unwrap(),
                vector["disposition"],
                "vector: {vector}"
            );
            assert_eq!(
                protocol.tool_family(),
                ToolFamily::NonFile,
                "vector: {vector}"
            );
            // Unchanged protocol fields keep old and new servers agreeing.
            assert_eq!(protocol.path_count(), 0);
            assert_eq!(protocol.extraction_state(), ExtractionState::Extracted);
        }
    }

    #[test]
    fn shell_tool_shapes_of_every_adapter_honor_exclusions() {
        let policy = shell_policy();
        for (agent, raw) in [
            (AgentKind::ClaudeCode, bash("cat docs/adr/x.md")),
            (
                AgentKind::Codex,
                json!({"tool_name": "shell", "tool_input": {"command": ["bash", "-lc", "cat docs/adr/x.md"]}}),
            ),
            (
                AgentKind::Codex,
                json!({"tool_name": "exec_command", "tool_input": {"cmd": "cat docs/adr/x.md"}}),
            ),
            (
                AgentKind::KiroCli,
                json!({"tool_name": "execute_bash", "tool_input": {"command": "cat docs/adr/x.md"}}),
            ),
            (
                AgentKind::Hermes,
                json!({"tool_name": "terminal", "tool_input": {"command": "cat docs/adr/x.md"}}),
            ),
            (
                AgentKind::OpenCode,
                json!({"tool": "bash", "args": {"command": "cat docs/adr/x.md"}}),
            ),
            (
                AgentKind::OpenClaw,
                json!({"tool": "exec", "args": {"command": "cat 0001.md", "workdir": "docs/adr"}}),
            ),
            (
                AgentKind::Devin,
                json!({"tool_name": "exec", "tool_input": {"command": "cat docs/adr/x.md"}}),
            ),
        ] {
            let decision = policy.inspect(agent, &raw, "/repo");
            if decision.protocol().tool_family() == ToolFamily::NonFile {
                assert_eq!(
                    decision.protocol().disposition(),
                    CaptureDisposition::Drop,
                    "raw: {raw}"
                );
            } else {
                // `exec_command` is not a recognized shell tool name; it keeps
                // today's unknown-tool behavior rather than guessing.
                assert_eq!(raw["tool_name"], "exec_command", "raw: {raw}");
                assert_eq!(decision.protocol().tool_family(), ToolFamily::Unknown);
            }
        }
    }

    /// Gemini CLI's `run_shell_command` and `read_many_files` fell to the
    /// unknown family, so `ignore_paths` never read their arguments and the
    /// raw output reached the spool. `read_many_files` takes globs that Gemini
    /// expands case-insensitively with a recursive `**`; any one that names or
    /// can expand into an ignored path drops the call, and glob syntax the
    /// matcher cannot evaluate fails closed.
    #[test]
    fn gemini_cli_shell_and_read_many_files_honor_ignored_paths() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            "/repo",
            None,
        );
        let gemini = |tool: &str, input: Value| {
            json!({"hook_event_name": "AfterTool", "tool_name": tool, "tool_input": input,
                "tool_response": {"llmContent": "SENTINEL_CONTENT"}})
        };
        let read_many = |include: &[&str]| gemini("read_many_files", json!({"include": include}));
        let shell = |input: Value| gemini("run_shell_command", input);
        for (raw, expected, family) in [
            (
                read_many(&["docs/adr/x.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["README.md", "docs/adr/x.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["docs/adr"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["docs/*/x.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["**/*.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["docs/**"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["/repo/**/x.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&["Docs/ADR/x.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&[r"docs\adr\x.md"]),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                read_many(&[
                    "src/**/*.rs",
                    "*.md",
                    "docs/guide.md",
                    "docs/adr-notes/x.md",
                ]),
                CaptureDisposition::Keep,
                ToolFamily::File,
            ),
            (
                read_many(&["src/**"]),
                CaptureDisposition::Keep,
                ToolFamily::File,
            ),
            (
                read_many(&["/elsewhere/**/*.md"]),
                CaptureDisposition::Keep,
                ToolFamily::File,
            ),
            // `exclude` cannot be trusted to subtract an ignored path.
            (
                gemini(
                    "read_many_files",
                    json!({"include": ["docs/**"], "exclude": ["docs/adr/**"]}),
                ),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            // Pre-rename spelling of the same tool.
            (
                gemini("read_many_files", json!({"paths": ["docs/adr/x.md"]})),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                gemini(
                    "read_many_files",
                    json!({"paths": ["README.md"], "include": ["docs/adr/*"]}),
                ),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            // Unprovable globs and shapes fail closed to metadata only.
            (
                read_many(&["docs/ad[r]/x.md"]),
                CaptureDisposition::MetadataOnly,
                ToolFamily::File,
            ),
            (
                read_many(&["docs/{adr,x}/*.md"]),
                CaptureDisposition::MetadataOnly,
                ToolFamily::File,
            ),
            (
                read_many(&["README.md", "!(x)"]),
                CaptureDisposition::MetadataOnly,
                ToolFamily::File,
            ),
            (
                read_many(&[]),
                CaptureDisposition::MetadataOnly,
                ToolFamily::File,
            ),
            (
                gemini("read_many_files", json!({"include": ["README.md", 7]})),
                CaptureDisposition::MetadataOnly,
                ToolFamily::File,
            ),
            (
                shell(json!({"command": "cat docs/adr/x.md"})),
                CaptureDisposition::Drop,
                ToolFamily::NonFile,
            ),
            (
                shell(json!({"command": "cat x.md", "dir_path": "docs/adr"})),
                CaptureDisposition::Drop,
                ToolFamily::NonFile,
            ),
            (
                shell(json!({"command": "cat x.md", "dir_path": "/repo/docs/adr"})),
                CaptureDisposition::Drop,
                ToolFamily::NonFile,
            ),
            (
                shell(json!({"command": "cat x.md", "directory": "docs/adr"})),
                CaptureDisposition::Drop,
                ToolFamily::NonFile,
            ),
            (
                shell(json!({"command": "cat README.md", "description": "show"})),
                CaptureDisposition::Keep,
                ToolFamily::NonFile,
            ),
            (
                shell(json!({"command": "cat x.md", "dir_path": "src"})),
                CaptureDisposition::Keep,
                ToolFamily::NonFile,
            ),
            (
                gemini("list_directory", json!({"dir_path": "docs"})),
                CaptureDisposition::Drop,
                ToolFamily::SearchList,
            ),
            (
                gemini("search_file_content", json!({"pattern": "x"})),
                CaptureDisposition::Drop,
                ToolFamily::SearchList,
            ),
            (
                gemini("grep_search", json!({"pattern": "x"})),
                CaptureDisposition::Drop,
                ToolFamily::SearchList,
            ),
            (
                gemini("glob", json!({"pattern": "**/*.md"})),
                CaptureDisposition::Drop,
                ToolFamily::SearchList,
            ),
            (
                gemini("read_file", json!({"file_path": "docs/adr/x.md"})),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                gemini(
                    "write_file",
                    json!({"file_path": "docs/adr/x.md", "content": "x"}),
                ),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                gemini(
                    "replace",
                    json!({"file_path": "docs/adr/x.md", "old_string": "a", "new_string": "b"}),
                ),
                CaptureDisposition::Drop,
                ToolFamily::File,
            ),
            (
                gemini("read_file", json!({"file_path": "README.md"})),
                CaptureDisposition::Keep,
                ToolFamily::File,
            ),
        ] {
            let decision = policy.inspect(AgentKind::GeminiCli, &raw, "/repo");
            assert_eq!(decision.protocol().disposition(), expected, "raw: {raw}");
            assert_eq!(decision.protocol().tool_family(), family, "raw: {raw}");
        }

        // Windows host: Gemini normalizes `\` and ignores case like the drive.
        let windows = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            r"C:\repo",
            None,
        );
        for (include, expected) in [
            (r"DOCS\adr\*.md", CaptureDisposition::Drop),
            (r"C:\repo\**\x.md", CaptureDisposition::Drop),
            (r"src\**\*.rs", CaptureDisposition::Keep),
        ] {
            let raw = read_many(&[include]);
            let decision = windows.inspect(AgentKind::GeminiCli, &raw, r"C:\repo");
            assert_eq!(decision.protocol().disposition(), expected, "{include}");
        }

        // An absolute `dir_path` locates the shell call for rerouting, exactly
        // like a `workdir`; a relative one cannot prove a checkout.
        assert_eq!(
            absolute_tool_location_paths(
                AgentKind::GeminiCli,
                &shell(json!({"command": "ls", "dir_path": "/other/repo"})),
                "/repo"
            ),
            Some(vec!["/other/repo".to_owned()])
        );
        assert_eq!(
            absolute_tool_location_paths(
                AgentKind::GeminiCli,
                &shell(json!({"command": "ls", "dir_path": "sub"})),
                "/repo"
            ),
            None
        );
    }

    /// Gemini CLI reads a directory entry of `read_many_files` as
    /// `entry/**`, so an entry naming an ancestor of an ignored directory
    /// (`docs`, `.`) reads the ignored files. The shared vectors attempt it
    /// through both payload shapes; `src` and `README.md` are the controls.
    #[test]
    fn read_many_files_directory_entries_cannot_reach_ignored_paths() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/capture-policy.json")).unwrap();
        let section = &fixture["read_many_files_directories"];
        let ignore_paths = section["ignore_paths"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pattern| pattern.as_str().unwrap().to_owned())
            .collect();
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig { ignore_paths }),
            "/repo",
            None,
        );
        for vector in section["vectors"].as_array().unwrap() {
            let include = &vector["include"];
            for (agent, raw) in [
                (
                    AgentKind::GeminiCli,
                    json!({"tool_name": "read_many_files", "tool_input": {"include": include}}),
                ),
                (
                    AgentKind::OpenCode,
                    json!({"tool": "read_many_files", "args": {"include": include}}),
                ),
            ] {
                let protocol = policy.inspect(agent, &raw, "/repo").protocol().clone();
                assert_eq!(
                    serde_json::to_value(protocol.disposition()).unwrap(),
                    vector["disposition"],
                    "{raw}"
                );
                assert_eq!(protocol.tool_family(), ToolFamily::File);
            }
        }
        assert!(glob_matches_below("/repo/docs/adr/**", "/repo", false));
        assert!(glob_matches_below("/**", "/", false));
        assert!(glob_matches_below("/repo/**/*.md", "/repo/src", false));
        assert!(!glob_matches_below("/repo/*.md", "/repo/README.md", false));
        assert!(!glob_matches_below("/repo/docs/adr/**", "/repo/src", false));
        assert!(glob_matches_below("C:/Docs/**", "c:/docs", true));
    }

    /// Tools that read back an earlier command's output are stripped to
    /// metadata under an active or invalid policy, since the command they
    /// read may have been dropped; with no policy they are untouched.
    #[test]
    fn output_reader_tools_are_metadata_only_only_under_a_policy() {
        let reader = json!({
            "tool_name": "BashOutput", "tool_input": {"bash_id": "b1"},
            "tool_response": "PRIVATE_BODY_SENTINEL",
        });
        let active = shell_policy();
        let invalid = CapturePolicy::resolve(CaptureSource::Invalid, "/repo", None);
        let inactive = CapturePolicy::resolve(CaptureSource::Absent, "/repo", None);
        for policy in [&active, &invalid] {
            let decision = policy.inspect(AgentKind::ClaudeCode, &reader, "/repo");
            assert_eq!(
                decision.protocol().disposition(),
                CaptureDisposition::MetadataOnly
            );
            let body = metadata_only_body(Some("s"), Some("/repo"), &decision).to_string();
            assert!(!body.contains("PRIVATE_BODY_SENTINEL"), "{body}");
        }
        assert_eq!(
            inactive
                .inspect(AgentKind::ClaudeCode, &reader, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
        // Control: an unrelated unknown tool keeps today's behavior.
        let other = json!({"tool_name": "FutureTool", "tool_input": {"bash_id": "b1"}});
        assert_eq!(
            active
                .inspect(AgentKind::ClaudeCode, &other, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
    }

    /// Copilot CLI's runtime `view`/`create` names are file tools for that
    /// agent only; another agent's same-named tool stays unknown.
    #[test]
    fn copilot_cli_runtime_file_tools_are_scoped_to_copilot() {
        assert_eq!(
            agent_family(AgentKind::CopilotCli, "view"),
            ToolFamily::File
        );
        assert_eq!(
            agent_family(AgentKind::CopilotCli, "create"),
            ToolFamily::File
        );
        assert_eq!(
            agent_family(AgentKind::CopilotCli, "str_replace_editor"),
            ToolFamily::File
        );
        assert_eq!(
            agent_family(AgentKind::CopilotCli, "rg"),
            ToolFamily::SearchList
        );
        assert_eq!(
            agent_family(AgentKind::CopilotCli, "edit"),
            ToolFamily::File
        );
        assert_eq!(
            agent_family(AgentKind::ClaudeCode, "view"),
            ToolFamily::Unknown
        );
        assert_eq!(
            agent_family(AgentKind::OpenCode, "create"),
            ToolFamily::Unknown
        );
        let metadata = tool_observation_metadata(
            AgentKind::CopilotCli,
            &json!({"tool_name": "view", "tool_input": {"path": "src/lib.rs"}}),
            true,
        )
        .unwrap();
        assert_eq!(metadata.tool_family, ToolFamily::File);
    }

    #[test]
    fn unevaluated_shell_expansions_respect_quoting() {
        for (command, expected) in [
            ("cat docs/{adr,x}/1.md", true),
            ("cat docs/ad[r]/1.md", true),
            ("echo {1..3}", true),
            ("cat 'docs/{adr,x}'", false),
            ("cat \"docs/[ab]\"", false),
            ("cat docs/\\{a,b}", false),
            ("cat docs/\\[ab]", false),
            ("echo ${a,b} ${arr[0]}", false),
            ("find . -exec cat {} ;", false),
            ("[ -f x ]", false),
            ("[[ -d x ]]", false),
            ("git diff HEAD@{1}..HEAD", false),
            ("echo a, b} {c", false),
            ("cat [] x", false),
            // A leading `]` is a class member; the class still closes.
            ("cat docs/ad[]r]/1.md", true),
            ("cat docs/ad[!]x]/1.md", true),
            ("jq .[] x.json", false),
            // zsh glob groups and extglobs, but not subshells or substitutions.
            ("cat docs/(adr|x)/1.md", true),
            ("cat (docs|x)/adr/1.md", true),
            ("cat (docs)/adr/1.md", true),
            ("cat (docs/adr/1.md|x)", true),
            ("cat docs/@(adr)/1.md", true),
            ("(cd src && cargo test)", false),
            ("(cd src && cargo test) | tee log", false),
            ("echo $((1+2)) $(date) x=$(pwd)/y", false),
            ("diff <(ls src) <(ls tests)", false),
            ("arr=(a b)", false),
            ("cat '(adr|x)'", false),
            // ANSI-C strings: numeric and control escapes are not decoded.
            ("cat $'docs/\\x61dr/1.md'", true),
            ("cat $'docs/\\141dr/1.md'", true),
            ("cat $'docs/\\u0061dr'", true),
            ("cat $'it\\'s {a,b}'", false),
            ("cat $'a\\tb'", false),
        ] {
            assert_eq!(has_unevaluated_expansion(command), expected, "{command}");
        }
    }

    #[test]
    fn globstar_reaches_lets_double_star_span_any_depth() {
        assert!(globstar_reaches("/repo/**/*.md", "/repo/docs/adr", false));
        assert!(globstar_reaches("/**", "/repo/docs/adr", false));
        assert!(globstar_reaches("/repo/d*/**", "/repo/docs/adr", false));
        assert!(globstar_reaches(
            "/repo/docs/adr/sub/**",
            "/repo/docs/adr",
            false
        ));
        assert!(globstar_reaches("C:/**/x", "C:/Docs", true));
        assert!(!globstar_reaches("/repo/src/**", "/repo/docs/adr", false));
        assert!(!globstar_reaches("/other/**", "/repo/docs/adr", false));
        // Without `**` it is exactly `glob_reaches`.
        assert!(!globstar_reaches("/repo/*.md", "/repo/docs/adr", false));
        assert!(globstar_reaches("/repo/*/adr/x", "/repo/docs/adr", false));
        // `glob_reaches` alone does not recurse; matching uses `globstar_reaches`.
        assert!(!glob_reaches("/repo/**/*.md", "/repo/docs/adr", false));
    }

    /// Claude Code's Windows `PowerShell` tool was classified unknown, so its
    /// output was captured without `ignore_paths` ever seeing the command
    /// (#1186). Once it is a shell tool, a command naming an ignored path in
    /// Windows spelling, including PowerShell's `-Param:value` binding, must
    /// drop; one naming a public path must still be kept.
    #[test]
    fn claude_code_powershell_honors_ignored_paths_on_a_windows_host() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            r"C:\repo",
            None,
        );
        let powershell = |command: &str| json!({"tool_name": "PowerShell", "tool_input": {"command": command}, "tool_use_id": "toolu_1"});
        for (command, expected) in [
            (r"Get-Content docs\adr\x.md", CaptureDisposition::Drop),
            (
                r"Get-Content -Path .\docs\adr\x.md",
                CaptureDisposition::Drop,
            ),
            (r"Get-Content -Path:docs\adr\x.md", CaptureDisposition::Drop),
            (r"type C:\repo\DOCS\ADR\x.md", CaptureDisposition::Drop),
            (r"Get-Content -Path:src\lib.rs", CaptureDisposition::Keep),
            ("git status", CaptureDisposition::Keep),
        ] {
            let decision = policy.inspect(AgentKind::ClaudeCode, &powershell(command), r"C:\repo");
            assert_eq!(
                decision.protocol().tool_family(),
                ToolFamily::NonFile,
                "{command}"
            );
            assert_eq!(decision.protocol().disposition(), expected, "{command}");
        }
        // A broken marker cannot prove a PowerShell command misses every
        // ignored path, exactly like Bash.
        let invalid = CapturePolicy::resolve(CaptureSource::Invalid, r"C:\repo", None);
        assert_eq!(
            invalid
                .inspect(AgentKind::ClaudeCode, &powershell("git status"), r"C:\repo")
                .protocol()
                .disposition(),
            CaptureDisposition::MetadataOnly,
        );
    }

    #[test]
    fn shell_matching_is_off_when_inactive_and_fails_closed_when_invalid_or_over_budget() {
        let raw = bash("cat docs/adr/x.md");
        let inactive = CapturePolicy::resolve(CaptureSource::Absent, "/repo", None);
        assert_eq!(
            inactive
                .inspect(AgentKind::ClaudeCode, &raw, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
        // A broken marker strips a shell call like a file call, but a
        // non-file tool with no command (`web_search`) keeps its body.
        let invalid = CapturePolicy::resolve(CaptureSource::Invalid, "/repo", None);
        let decision = invalid.inspect(AgentKind::ClaudeCode, &raw, "/repo");
        assert_eq!(
            decision.protocol().disposition(),
            CaptureDisposition::MetadataOnly
        );
        let body = metadata_only_body(Some("s"), Some("/repo"), &decision).to_string();
        assert!(!body.contains("docs/adr"), "{body}");
        assert_eq!(
            invalid
                .inspect(
                    AgentKind::ClaudeCode,
                    &json!({"tool_name": "web_search", "tool_input": {"query": "docs/adr"}}),
                    "/repo"
                )
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
        // Without a home directory `~/` is left literal and cannot match.
        let homeless = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            "/repo",
            None,
        );
        assert_eq!(
            homeless
                .inspect(AgentKind::ClaudeCode, &bash("cat ~/notes/x.md"), "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
        let costly = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/**/?".repeat(100)],
            }),
            "/repo",
            None,
        );
        let long = format!("cat {}", vec!["docs/x".repeat(400); 8].join(" "));
        assert_eq!(
            costly
                .inspect(AgentKind::ClaudeCode, &bash(&long), "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Drop
        );
    }

    #[test]
    fn shell_words_follow_posix_quoting_and_keep_windows_separators() {
        assert_eq!(
            shell_words(r#"a 'b c' "d \"e\"" f\ g h|i>j;k&&(l) C:\docs\adr\x.md"#),
            [
                "a",
                "b c",
                "d \"e\"",
                "f g",
                "h",
                "i",
                "j",
                "k",
                "l",
                r"C:\docs\adr\x.md"
            ]
        );
        assert_eq!(literal_prefix("/repo/docs/adr/**"), "/repo/docs/adr");
        assert_eq!(literal_prefix("/**"), "/");
        assert_eq!(literal_prefix("C:/*.md"), "C:/");
        assert_eq!(literal_prefix("/repo/secret.txt"), "/repo/secret.txt");
        assert!(glob_reaches("/repo/docs/*/x.md", "/repo/docs/adr", false));
        assert!(!glob_reaches("/repo/*.md", "/repo/docs/adr", false));
        assert!(glob_reaches("C:/Docs/*", "C:/docs/adr", true));
    }

    #[test]
    fn metadata_rewrite_strips_all_sentinels_and_has_exact_protocol_keys() {
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["secret/**".into()],
            }),
            "/repo",
            None,
        );
        let raw = json!({"tool_name":"Edit","tool_call_id":"safe-ID.1","tool_input":{"path":"secret/SENTINEL_PATH","args":"SENTINEL_ARGS"},"output":"SENTINEL_OUTPUT","error":"SENTINEL_ERROR","title":"SENTINEL_TITLE","result":"SENTINEL_RESULT","nested":{"raw":"SENTINEL_NESTED"}});
        let body = metadata_only_body(
            Some("s"),
            Some("/repo"),
            &policy.inspect(AgentKind::Codex, &raw, "/repo"),
        );
        let text = body.to_string();
        for sentinel in [
            "SENTINEL_PATH",
            "SENTINEL_ARGS",
            "SENTINEL_OUTPUT",
            "SENTINEL_ERROR",
            "SENTINEL_TITLE",
            "SENTINEL_RESULT",
            "SENTINEL_NESTED",
        ] {
            assert!(!text.contains(sentinel));
        }
        let keys: Vec<_> = body["_ai_memory_capture"]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(
            keys,
            [
                "version",
                "disposition",
                "policy_state",
                "tool_family",
                "path_count",
                "extraction_state"
            ]
        );
    }
    #[test]
    fn invalid_marker_strips_shell_calls_with_unparseable_commands() {
        let policy = CapturePolicy::resolve(CaptureSource::Invalid, "/repo", None);
        for (agent, payload) in [
            (
                AgentKind::ClaudeCode,
                json!({"tool_name":"Bash","tool_input":{"command":7}}),
            ),
            (
                AgentKind::Codex,
                json!({"tool_name":"shell","tool_input":{"command":["cat", 7]}}),
            ),
            (
                AgentKind::ClaudeCode,
                json!({"tool_name":"Bash","tool_input":{"cmd_line":"cat secret/x.md"}}),
            ),
        ] {
            let decision = policy.inspect(agent, &payload, "/repo");
            assert_eq!(
                decision.protocol().disposition(),
                CaptureDisposition::MetadataOnly,
                "{payload}"
            );
        }
    }

    #[test]
    fn long_bash_lc_script_in_argv_is_not_dropped_by_the_match_budget() {
        let ignore_paths = (0..40).map(|i| format!("/repo/private{i}/**")).collect();
        let policy = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig { ignore_paths }),
            "/repo",
            None,
        );
        assert_eq!(policy.state, PolicyState::Active);
        let script = format!("{}ls a?.rs", "echo x; ".repeat(350));
        assert!(script.chars().count() < MAX_CANDIDATE_PATH_CHARS);
        // Control: as a string, the script splits into short words.
        let as_string = json!({"tool_name":"shell","tool_input":{"command": script}});
        assert_eq!(
            policy
                .inspect(AgentKind::Codex, &as_string, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep,
            "control"
        );
        let as_argv = json!({"tool_name":"shell","tool_input":{"command":["bash","-lc",script]}});
        assert_eq!(
            policy
                .inspect(AgentKind::Codex, &as_argv, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep,
            "innocuous script silently dropped by the match budget"
        );
    }
    #[test]
    fn shell_command_borrows_the_command_and_splits_only_on_request() {
        let line = json!({"command": "cat 'private notes/x.md' | head"});
        let Some(ShellCommand::Line(borrowed)) = shell_command(&line) else {
            panic!("a string command is kept as a line");
        };
        assert!(std::ptr::eq(borrowed, line["command"].as_str().unwrap()));
        // A quoted word holding several words is read like a command line.
        assert_eq!(
            ShellCommand::Line(borrowed).words(),
            ["cat", "private notes/x.md", "private", "notes/x.md", "head"]
        );

        let argv = json!({"cmd": ["cat", "private notes/x.md"]});
        assert_eq!(
            shell_command(&argv).unwrap().words(),
            ["cat", "private notes/x.md", "private", "notes/x.md"]
        );
        let script = format!("echo {}; ls", "x ".repeat(MAX_ARGV_PATH_CHARS));
        let argv = json!({"command": ["bash", "-lc", script]});
        let words = shell_command(&argv).unwrap().words();
        assert!(!words.contains(&script), "a long script is not one path");
        assert_eq!(words.last().map(String::as_str), Some("ls"));

        for args in [
            json!({"command": 7}),
            json!({"command": ["cat", 7]}),
            json!({"cmd_line": "cat x"}),
        ] {
            assert!(shell_command(&args).is_none(), "{args}");
        }
    }
    /// A string-form command used to keep any quoted script whole, so
    /// `bash -c 'cat <ignored>'` or `"$(cat <ignored>)"` was kept while the
    /// argv form dropped. Nested words are now read like a command line, to a
    /// bounded depth; deeper nesting fails closed.
    #[test]
    fn quoted_scripts_in_string_commands_are_read_like_command_lines() {
        let policy = shell_policy();
        let decide = |command: &str| {
            policy
                .inspect(AgentKind::ClaudeCode, &bash(command), "/repo")
                .protocol()
                .disposition()
        };
        for command in [
            "bash -c 'cat docs/adr/1.md'",
            "timeout 5 bash -c \"head docs/adr/1.md\"",
            "echo \"$(cat docs/adr/1.md)\"",
            "FOO=\"$(cat docs/adr/1.md)\"",
            "echo \"`cat docs/adr/1.md`\"",
            "bash -c \"sh -c 'cat docs/adr/1.md'\"",
            "python3 -c \"print(open('docs/adr/1.md').read())\"",
        ] {
            assert_eq!(decide(command), CaptureDisposition::Drop, "{command}");
        }
        for command in ["bash -c 'cargo test'", "echo \"$(git rev-parse HEAD)\""] {
            assert_eq!(decide(command), CaptureDisposition::Keep, "{command}");
        }
        // Five quoting levels: the innermost script is not read, so the
        // command proves nothing.
        let mut script = "cat docs/adr/1.md".to_owned();
        for _ in 0..MAX_SHELL_NESTING {
            script = format!("bash -c {}", shell_quote(&script));
        }
        assert_eq!(decide(&script), CaptureDisposition::Drop, "four levels");
        let deeper = format!(
            "bash -c {}",
            shell_quote(&script.replace("docs/adr", "src"))
        );
        assert_eq!(
            decide(&deeper),
            CaptureDisposition::MetadataOnly,
            "five levels"
        );
    }

    fn shell_quote(text: &str) -> String {
        format!("'{}'", text.replace('\'', "'\\''"))
    }

    /// Inline interpreter code can name a path no lexical match sees, so a
    /// command running it is metadata-only unless a literal names an ignored
    /// path outright. Running a script file or a module is unaffected.
    #[test]
    fn inline_interpreter_code_is_metadata_only_under_an_active_policy() {
        for (words, inline) in [
            (&["python3", "-c", "x"][..], true),
            (&["/usr/bin/python3.12", "-I", "-c", "x"], true),
            (&["python", "-Ic", "x"], true),
            (&["node", "-e", "x"], true),
            (&["node", "--eval=x"], true),
            (&["bun", "-p", "x"], true),
            (&["ruby", "-e", "x"], true),
            (&["perl", "-ne", "x"], true),
            (&["php", "-r", "x"], true),
            (&["deno", "eval", "x"], true),
            (&["python3", "-m", "pytest", "-c", "x"], false),
            (&["python3", "script.py"], false),
            (&["node", "scripts/build.js"], false),
            (&["ruby", "-Eutf-8", "x.rb"], false),
            (&["grep", "-e", "x"], false),
        ] {
            assert_eq!(runs_inline_code(words), inline, "{words:?}");
        }
        let policy = shell_policy();
        let argv =
            json!({"tool_name": "shell", "tool_input": {"command": ["python3", "-c", "print(1)"]}});
        assert_eq!(
            policy
                .inspect(AgentKind::Codex, &argv, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::MetadataOnly
        );
        let inactive = CapturePolicy::resolve(CaptureSource::Absent, "/repo", None);
        assert_eq!(
            inactive
                .inspect(AgentKind::Codex, &argv, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
    }

    /// `git show REV:path` names a file the way `cat path` does, resolved
    /// from the repository root; drive letters, URLs and host names are not
    /// object names.
    #[test]
    fn git_object_names_are_path_candidates() {
        assert_eq!(git_object_path("HEAD:docs/x.md"), Some("docs/x.md"));
        assert_eq!(git_object_path(":docs/x.md"), Some("docs/x.md"));
        assert_eq!(git_object_path(":2:docs/x.md"), Some("docs/x.md"));
        assert_eq!(git_object_path("HEAD@{1}:x"), Some("x"));
        assert_eq!(git_object_path(r"C:\docs\x.md"), None);
        assert_eq!(git_object_path("C:/docs/x.md"), None);
        assert_eq!(git_object_path("https://h/docs/x.md"), None);
        assert_eq!(git_object_path("NAME=a:b"), None);
        assert_eq!(git_object_path("--format=%h:%s"), None);
        let policy = shell_policy();
        for (command, cwd, expected) in [
            (
                "git show HEAD:docs/adr/1.md",
                "/repo",
                CaptureDisposition::Drop,
            ),
            (
                "git show HEAD:docs/adr/1.md",
                "/repo/src",
                CaptureDisposition::Drop,
            ),
            (
                "git show :./adr/1.md",
                "/repo/docs",
                CaptureDisposition::Drop,
            ),
            (
                "git show HEAD:src/main.rs",
                "/repo",
                CaptureDisposition::Keep,
            ),
        ] {
            assert_eq!(
                policy
                    .inspect(AgentKind::ClaudeCode, &bash(command), cwd)
                    .protocol()
                    .disposition(),
                expected,
                "{command} in {cwd}"
            );
        }
        // A Windows host keeps its drive paths out of object-name parsing.
        let windows = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            r"C:\repo",
            None,
        );
        for (command, expected) in [
            (r"type C:\repo\src\x.md", CaptureDisposition::Keep),
            ("git show HEAD:docs/adr/1.md", CaptureDisposition::Drop),
        ] {
            assert_eq!(
                windows
                    .inspect(AgentKind::ClaudeCode, &bash(command), r"C:\repo")
                    .protocol()
                    .disposition(),
                expected,
                "{command}"
            );
        }
    }

    /// `$'…'` is decoded like the shell does, and `\o` outside quotes is
    /// `o`, so neither spelling hides an ignored path.
    #[test]
    fn ansi_c_strings_and_escaped_letters_are_read_like_the_shell() {
        assert_eq!(
            shell_words(r"cat $'docs/adr/1.md' $'a\tb' $'it\'s' $'\x61' $loc"),
            ["cat", "docs/adr/1.md", "a\tb", "it's", r"\x61", "$loc"]
        );
        assert_eq!(shell_words("echo `cat x`y"), ["echo", "cat", "x", "y"]);
        let policy = shell_policy();
        for (command, expected) in [
            ("cat $'docs/adr/1.md'", CaptureDisposition::Drop),
            ("cat $\"docs/adr/1.md\"", CaptureDisposition::Drop),
            (r"cat d\ocs/adr/1.md", CaptureDisposition::Drop),
            (r"cat docs/a\dr/1.\md", CaptureDisposition::Drop),
            (r"cat $'docs/\x61dr/1.md'", CaptureDisposition::MetadataOnly),
            ("cat $'src/main.rs'", CaptureDisposition::Keep),
            (r"cat s\rc/main.rs", CaptureDisposition::Keep),
        ] {
            assert_eq!(
                policy
                    .inspect(AgentKind::ClaudeCode, &bash(command), "/repo")
                    .protocol()
                    .disposition(),
                expected,
                "{command}"
            );
        }
    }

    /// Antigravity's `run_command` runs in its `Cwd` argument, so a relative
    /// argument resolves from there; another agent's `Cwd` key means nothing.
    #[test]
    fn antigravity_run_command_resolves_arguments_from_its_cwd() {
        let policy = shell_policy();
        let run = |command: &str, cwd: &str| json!({"toolCall": {"name": "run_command", "args": {"CommandLine": command, "Cwd": cwd}}});
        for (raw, expected) in [
            (run("cat 1.md", "/repo/docs/adr"), CaptureDisposition::Drop),
            (run("cat adr/1.md", "docs"), CaptureDisposition::Drop),
            (run("cat 1.md", "/repo/src"), CaptureDisposition::Keep),
            (run("cat docs/adr/1.md", "/repo"), CaptureDisposition::Drop),
        ] {
            assert_eq!(
                policy
                    .inspect(AgentKind::AntigravityCli, &raw, "/repo")
                    .protocol()
                    .disposition(),
                expected,
                "{raw}"
            );
        }
        let other = json!({"tool_name": "Bash", "tool_input": {"command": "cat 1.md", "Cwd": "/repo/docs/adr"}});
        assert_eq!(
            policy
                .inspect(AgentKind::ClaudeCode, &other, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
        assert_eq!(
            absolute_tool_location_paths(
                AgentKind::AntigravityCli,
                &run("ls", "/other/repo"),
                "/repo"
            ),
            Some(vec!["/other/repo".to_owned()])
        );
    }
    /// On a Windows host cmd's caret and PowerShell's backquote escape the
    /// next character whatever the shell tool, so their readings without the
    /// escape are matched; a POSIX shell reads both literally (control).
    #[test]
    fn windows_escape_characters_are_read_on_a_windows_host() {
        let windows = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            r"C:\repo",
            None,
        );
        for (command, expected) in [
            (r"type docs\ad^r\x.md", CaptureDisposition::Drop),
            (r"Get-Content docs\ad`r\x.md", CaptureDisposition::Drop),
            (r"type C:\repo\DOCS\A^DR\x.md", CaptureDisposition::Drop),
            (r"type src\ma^in.rs", CaptureDisposition::Keep),
            ("echo a^b", CaptureDisposition::Keep),
        ] {
            let decision = windows.inspect(AgentKind::ClaudeCode, &bash(command), r"C:\repo");
            assert_eq!(decision.protocol().disposition(), expected, "{command}");
        }
        let posix = CapturePolicy::resolve(
            CaptureSource::Parsed(&CaptureConfig {
                ignore_paths: vec!["docs/adr/**".into()],
            }),
            "/repo",
            None,
        );
        for command in ["cat docs/ad^r/x.md", "cat docs/ad`r`/x.md"] {
            assert_eq!(
                posix
                    .inspect(AgentKind::ClaudeCode, &bash(command), "/repo")
                    .protocol()
                    .disposition(),
                CaptureDisposition::Keep,
                "{command}"
            );
        }
    }

    /// A macOS host reads `DOCS/ADR/x.md` as `docs/adr/x.md`, so file tools
    /// and shell arguments match without regard to case there, and only
    /// there (control: the same host-independent policy elsewhere).
    #[test]
    fn a_case_folding_host_matches_paths_without_regard_to_case() {
        let policy = |fold_case| {
            CapturePolicy::resolve_for_host(
                CaptureSource::Parsed(&CaptureConfig {
                    ignore_paths: vec!["docs/adr/**".into()],
                }),
                "/repo",
                None,
                fold_case,
            )
        };
        let read = json!({"tool_name": "Read", "tool_input": {"file_path": "DOCS/Adr/x.md"}});
        for (raw, label) in [(read, "read"), (bash("cat DOCS/Adr/x.md"), "bash")] {
            for (fold_case, expected) in [
                (true, CaptureDisposition::Drop),
                (false, CaptureDisposition::Keep),
            ] {
                assert_eq!(
                    policy(fold_case)
                        .inspect(AgentKind::ClaudeCode, &raw, "/repo")
                        .protocol()
                        .disposition(),
                    expected,
                    "{label} fold_case={fold_case}"
                );
            }
        }
        let public = json!({"tool_name": "Read", "tool_input": {"file_path": "SRC/x.md"}});
        assert_eq!(
            policy(true)
                .inspect(AgentKind::ClaudeCode, &public, "/repo")
                .protocol()
                .disposition(),
            CaptureDisposition::Keep
        );
        assert!(
            !CapturePolicy::resolve(CaptureSource::Absent, "/repo", None).fold_case,
            "an inactive policy never folds"
        );
        assert_eq!(
            CapturePolicy::resolve(
                CaptureSource::Parsed(&CaptureConfig {
                    ignore_paths: vec!["x/**".into()],
                }),
                "/repo",
                None,
            )
            .fold_case,
            cfg!(target_os = "macos")
        );
    }

    /// Reading nested words and the new fail-closed syntax must not strip
    /// everyday commands that name no ignored path.
    #[test]
    fn common_commands_stay_kept_under_an_active_policy() {
        let policy = shell_policy();
        for command in [
            "cargo test",
            "cargo test -p ai-memory-hooks -- --nocapture",
            "cargo clippy --workspace --all-targets -- -D warnings",
            "git status",
            "git log --oneline -10",
            "git log --format='%h %s' -5",
            "git log --pretty=format:\"%h {a,b}\" -- src/",
            "git diff",
            "git diff --stat HEAD~1",
            "git diff HEAD~1 -- src/",
            "git show HEAD --stat",
            "git show HEAD:src/main.rs",
            "git commit -m \"fix: handle {a,b} and [x] in the parser\"",
            "ls",
            "ls -la src",
            "npm test",
            "npm run build -- --watch",
            "npx jest src/app.test.ts",
            "pytest -k \"not slow\" -x tests/",
            "python3 -m pytest -q",
            "python3 scripts/gen.py --out build/",
            "node scripts/build.js",
            "rg TODO src",
            "rg \"(foo|bar)baz\" src",
            "grep -rn \"fn main\\(\" src",
            "echo \"$(git rev-parse HEAD)\"",
            "(cd src && cargo test)",
            "docker compose up -d",
            "curl -s http://localhost:8080/health",
            // Near misses of the continuation, option-value, glued-flag,
            // URL, tilde and parameter-default readings.
            "cargo test \\\n  --workspace",
            "bash -o pipefail -c 'cargo test'",
            "set -o pipefail; cargo test",
            "python3 -W ignore -m pytest",
            "node -r ts-node/register scripts/build.ts",
            "grep -fpatterns.txt src",
            "cargo build -j4",
            "tar -czf out.tgz src",
            "gcc -Isrc -o build/app src/main.c",
            "make -C src",
            "curl -s file:///tmp/report.txt",
            "cat ~+/src/main.rs",
            "ls -la ~",
            "echo ${HOME:-/tmp} ${PWD}",
            "git log --oneline HEAD~3..HEAD",
            "git rebase -i HEAD~2",
            "echo a^b",
        ] {
            assert_eq!(
                policy
                    .inspect(AgentKind::ClaudeCode, &bash(command), "/repo")
                    .protocol()
                    .disposition(),
                CaptureDisposition::Keep,
                "{command}"
            );
        }
    }
}
