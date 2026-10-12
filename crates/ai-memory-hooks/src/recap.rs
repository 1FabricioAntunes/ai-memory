//! `GET /recap`: the stretch of a session that its last compaction dropped,
//! rebuilt from captured observations for a "reload context" prompt.

use ai_memory_core::scaffolding::{looks_like_codex_instructions, looks_like_scaffolding};
use ai_memory_store::ObservationRecord;

use crate::router::{UNTRUSTED_HISTORY_END, UNTRUSTED_HISTORY_START};

/// Default recap size: under the 10,000-char cap Claude Code and Grok put on
/// one `additionalContext` field.
pub(crate) const RECAP_BUDGET_DEFAULT: usize = 8_000;
const RECAP_BUDGET_MIN: usize = 2_000;
const RECAP_BUDGET_MAX: usize = 9_500;
/// The last reply and the handoffs each keep this much room when present,
/// so a long run of prompts cannot crowd them out.
const REPLY_FLOOR: usize = 1_000;
const HANDOFF_FLOOR: usize = 500;
/// Room kept for the "N earlier prompts omitted" line once prompts overflow.
const OMITTED_LINE_RESERVE: usize = 80;
const TRUNCATED: &str = "\n[truncated]";

/// One captured prompt or reply, with its capture time.
#[derive(Debug, Clone)]
pub(crate) struct RecapItem {
    pub at: String,
    pub text: String,
}

/// The stretch of a session that ended at its latest compaction.
#[derive(Debug, Clone)]
pub(crate) struct RecapWindow {
    /// The operator's prompts in the window, oldest first.
    pub prompts: Vec<RecapItem>,
    /// The newest captured reply in the window.
    pub last_reply: Option<RecapItem>,
    /// When the latest compaction was captured.
    pub boundary_at: String,
    /// When the window starts: the previous compaction, else the oldest row
    /// read. `None` when the boundary is the only row.
    pub started_at: Option<String>,
    /// The read hit its row limit before reaching the window's start.
    pub truncated: bool,
}

/// An open handoff as the recap lists it.
#[derive(Debug, Clone)]
pub(crate) struct RecapHandoff {
    pub summary: String,
    pub next_steps: Vec<String>,
    pub created_at: String,
    /// Written by the session being recapped.
    pub own: bool,
}

/// The budget a client asked for, clamped; the default when absent or not a
/// number.
pub(crate) fn clamp_budget(raw: Option<&str>) -> usize {
    raw.and_then(|value| value.trim().parse::<usize>().ok())
        .map_or(RECAP_BUDGET_DEFAULT, |n| {
            n.clamp(RECAP_BUDGET_MIN, RECAP_BUDGET_MAX)
        })
}

/// Pick the window from `user-prompt`, `stop` and `pre-compact` rows read
/// newest first: everything between the latest `pre-compact` and the one
/// before it (or the oldest row read). `None` when the session has no
/// compaction. `limit_hit` says the read stopped at its row limit, so a
/// window without a starting boundary may be missing its oldest rows.
pub(crate) fn select_window(
    rows_desc: &[ObservationRecord],
    limit_hit: bool,
) -> Option<RecapWindow> {
    let boundary = rows_desc.iter().position(|r| r.kind == "pre-compact")?;
    let mut prompts = Vec::new();
    let mut last_reply = None;
    let mut started_at = None;
    let mut reached_start = false;
    for row in &rows_desc[boundary + 1..] {
        match row.kind.as_str() {
            "pre-compact" => {
                started_at = Some(row.created_at.clone());
                reached_start = true;
                break;
            }
            "user-prompt" if is_operator_prompt(&row.body) => prompts.push(item(row)),
            "stop" if last_reply.is_none() && !row.body.trim().is_empty() => {
                last_reply = Some(item(row));
            }
            _ => {}
        }
    }
    if !reached_start && rows_desc.len() > boundary + 1 {
        started_at = rows_desc.last().map(|r| r.created_at.clone());
    }
    prompts.reverse();
    Some(RecapWindow {
        prompts,
        last_reply,
        boundary_at: rows_desc[boundary].created_at.clone(),
        started_at,
        truncated: !reached_start && limit_hit,
    })
}

/// Harness markup, task notifications and Codex's injected instructions are
/// captured as prompts but were never typed by the operator.
fn is_operator_prompt(body: &str) -> bool {
    !looks_like_scaffolding(body) && !looks_like_codex_instructions(body)
}

fn item(row: &ObservationRecord) -> RecapItem {
    RecapItem {
        at: row.created_at.clone(),
        text: row.body.trim().to_owned(),
    }
}

/// Render the recap within `budget` chars. Prompts land uncut when they fit;
/// past the room left after the reply and handoff floors, the oldest are
/// dropped first and only a lone prompt larger than the room is clipped.
pub(crate) fn compose_recap(
    session_id: &str,
    window: &RecapWindow,
    handoffs: &[RecapHandoff],
    budget: usize,
) -> String {
    let header = recap_header(session_id, window);
    let footer = format!(
        "\nFull record: `memory_read_session_observations` with `session_id: \"{session_id}\"` and `kinds: [\"user-prompt\", \"stop\"]`.\n"
    );
    let prompts_heading = "\n## Your prompts before the compaction (oldest first)\n";
    let reply_heading = "\n## The agent's last reply before the compaction\n";
    let handoffs_heading = "\n## Open handoffs in this project (none claimed)\n";
    let not_read = if window.truncated {
        "\n_Earlier rows of this window were not read._\n"
    } else {
        ""
    };
    let reply = window.last_reply.as_ref().map_or_else(
        || {
            "_Replies were not captured. Install the hooks with `--capture-assistant` and enable `capture_assistant` on the server to include them._\n".to_owned()
        },
        |r| format!("{}\n", r.text),
    );
    let handoff_text = render_handoffs(handoffs);

    let fixed = [
        header.as_str(),
        UNTRUSTED_HISTORY_START,
        "\n",
        not_read,
        prompts_heading,
        reply_heading,
        if handoffs.is_empty() {
            ""
        } else {
            handoffs_heading
        },
        UNTRUSTED_HISTORY_END,
        "\n",
        footer.as_str(),
    ]
    .iter()
    .map(|part| chars(part))
    .sum::<usize>();
    let available = budget.saturating_sub(fixed);
    let reply_floor = chars(&reply).min(REPLY_FLOOR);
    let handoff_floor = chars(&handoff_text).min(HANDOFF_FLOOR);
    let prompt_room = available.saturating_sub(reply_floor + handoff_floor);

    let prompt_section = render_prompts(&window.prompts, prompt_room);
    let mut remaining = available.saturating_sub(chars(&prompt_section));
    let reply_room = remaining.saturating_sub(handoff_floor).min(chars(&reply));
    let reply = clip(&reply, reply_room);
    remaining = remaining.saturating_sub(chars(&reply));
    let handoff_text = clip(&handoff_text, remaining);

    let mut out = String::with_capacity(budget);
    out.push_str(&header);
    out.push_str(UNTRUSTED_HISTORY_START);
    out.push('\n');
    out.push_str(not_read);
    out.push_str(prompts_heading);
    out.push_str(&prompt_section);
    out.push_str(reply_heading);
    out.push_str(&reply);
    if !handoffs.is_empty() {
        out.push_str(handoffs_heading);
        out.push_str(&handoff_text);
    }
    out.push_str(UNTRUSTED_HISTORY_END);
    out.push('\n');
    out.push_str(&footer);
    clip(&out, budget)
}

fn recap_header(session_id: &str, window: &RecapWindow) -> String {
    let from = window
        .started_at
        .as_deref()
        .map_or_else(|| "the session start".to_owned(), utc_minute);
    format!(
        "> 🧭 **ai-memory: compaction recap** (requested with \"reload context\")\n\
         > **Security boundary:** {notice}\n\n\
         Session `{session_id}`: what was captured from {from} to the compaction at {to}. \
         Session page: `sessions/{session_id}.md`.\n\n",
        notice = ai_memory_core::UNTRUSTED_MEMORY_NOTICE,
        to = utc_minute(&window.boundary_at),
    )
}

fn render_prompts(prompts: &[RecapItem], room: usize) -> String {
    if prompts.is_empty() {
        return "_No prompts were captured in this window._\n".to_owned();
    }
    let lines: Vec<String> = prompts
        .iter()
        .map(|p| format!("- [{}] {}\n", utc_minute(&p.at), p.text))
        .collect();
    if lines.iter().map(|l| chars(l)).sum::<usize>() <= room {
        return lines.concat();
    }
    let room = room.saturating_sub(OMITTED_LINE_RESERVE);
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0;
    for line in lines.iter().rev() {
        let len = chars(line);
        if used + len <= room {
            kept.push(line.clone());
            used += len;
        } else if kept.is_empty() {
            let mut clipped = clip(line, room.saturating_sub(1));
            clipped.push('\n');
            used += chars(&clipped);
            kept.push(clipped);
        } else {
            break;
        }
    }
    kept.reverse();
    let omitted = lines.len() - kept.len();
    let mut out = String::new();
    if omitted > 0 {
        out.push_str(&format!(
            "- _{omitted} earlier prompts omitted to fit; see the full record below._\n"
        ));
    }
    out.push_str(&kept.concat());
    out
}

fn render_handoffs(handoffs: &[RecapHandoff]) -> String {
    let mut ordered: Vec<&RecapHandoff> = handoffs.iter().collect();
    ordered.sort_by_key(|h| !h.own);
    ordered
        .iter()
        .map(|h| {
            let next = if h.next_steps.is_empty() {
                String::new()
            } else {
                format!(" Next: {}.", h.next_steps.join("; "))
            };
            let whose = if h.own { ", from this session" } else { "" };
            format!(
                "- {}{next} (created {}{whose})\n",
                h.summary.trim(),
                utc_minute(&h.created_at)
            )
        })
        .collect()
}

/// A notice for a session that has no compaction yet.
pub(crate) fn no_compaction_notice(session_id: &str) -> String {
    format!(
        "> 🧭 **ai-memory: compaction recap**: no compaction is recorded for session `{session_id}` yet, so there is nothing to reload.\n"
    )
}

/// A notice for a session the store does not know, or one the caller may
/// not read (the two answer alike so an id cannot probe another operator).
pub(crate) fn no_session_notice() -> String {
    "> 🧭 **ai-memory: compaction recap**: no session is recorded under this id for you, so there is nothing to reload.\n".to_owned()
}

/// `2026-10-12T01:00:57.123Z` → `2026-10-12 01:00 UTC`; anything shorter is
/// shown as stored.
fn utc_minute(stamp: &str) -> String {
    match (stamp.get(..10), stamp.get(11..16)) {
        (Some(day), Some(minute)) => format!("{day} {minute} UTC"),
        _ => stamp.to_owned(),
    }
}

fn chars(text: &str) -> usize {
    text.chars().count()
}

/// Keep `text` within `max` chars, marking a cut.
fn clip(text: &str, max: usize) -> String {
    if chars(text) <= max {
        return text.to_owned();
    }
    let keep = max.saturating_sub(chars(TRUNCATED));
    let mut out: String = text.chars().take(keep).collect();
    if max >= chars(TRUNCATED) {
        out.push_str(TRUNCATED);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ai_memory_core::{ObservationId, SessionId};
    use ai_memory_store::ObservationRecord;

    fn row(kind: &str, body: &str, at: &str) -> ObservationRecord {
        ObservationRecord {
            id: ObservationId::new(),
            session_id: SessionId::new(),
            kind: kind.to_owned(),
            title: kind.to_owned(),
            body: body.to_owned(),
            importance: 5,
            created_at: at.to_owned(),
            extension: None,
            source_event: None,
        }
    }

    fn prompts(window: &RecapWindow) -> Vec<&str> {
        window.prompts.iter().map(|p| p.text.as_str()).collect()
    }

    fn handoff(summary: &str, own: bool) -> RecapHandoff {
        RecapHandoff {
            summary: summary.to_owned(),
            next_steps: vec!["take the screenshots".to_owned()],
            created_at: "2026-10-12T01:30:57Z".to_owned(),
            own,
        }
    }

    /// Newest first, as the endpoint reads them: two boundaries, the window
    /// between them, one prompt before and one after.
    fn two_boundaries() -> Vec<ObservationRecord> {
        vec![
            row("user-prompt", "after", "2026-10-12T10:55:29Z"),
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("stop", "r2", "2026-10-12T02:01:05Z"),
            row(
                "user-prompt",
                "<task-notification>\n<summary>Goal check-in</summary>\n</task-notification>",
                "2026-10-12T02:01:05Z",
            ),
            row("stop", "r1", "2026-10-12T01:30:57Z"),
            row("user-prompt", "p2", "2026-10-12T01:07:07Z"),
            row("user-prompt", "p1", "2026-10-12T01:00:57Z"),
            row("pre-compact", "", "2026-10-12T00:55:25Z"),
            row("user-prompt", "before", "2026-10-12T00:54:26Z"),
        ]
    }

    #[test]
    fn window_is_between_the_last_two_boundaries() {
        let window = select_window(&two_boundaries(), false).unwrap();
        assert_eq!(prompts(&window), ["p1", "p2"]);
        assert_eq!(window.last_reply.as_ref().unwrap().text, "r2");
        assert_eq!(window.boundary_at, "2026-10-12T02:55:02Z");
        assert_eq!(window.started_at.as_deref(), Some("2026-10-12T00:55:25Z"));
        assert!(!window.truncated);
    }

    #[test]
    fn no_boundary_returns_none() {
        let rows = vec![
            row("stop", "r", "2026-10-12T01:30:57Z"),
            row("user-prompt", "p", "2026-10-12T01:00:57Z"),
        ];
        assert!(select_window(&rows, false).is_none());
    }

    #[test]
    fn single_boundary_reads_back_to_session_start() {
        let rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("user-prompt", "p2", "2026-10-12T01:07:07Z"),
            row("user-prompt", "p1", "2026-10-12T01:00:57Z"),
        ];
        let window = select_window(&rows, false).unwrap();
        assert_eq!(prompts(&window), ["p1", "p2"]);
        assert_eq!(window.started_at.as_deref(), Some("2026-10-12T01:00:57Z"));
        assert!(window.last_reply.is_none());
    }

    #[test]
    fn codex_instructions_and_scaffolding_are_dropped() {
        let rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("user-prompt", "real", "2026-10-12T01:09:00Z"),
            row(
                "user-prompt",
                "<system-reminder>x</system-reminder>",
                "2026-10-12T01:08:00Z",
            ),
            row(
                "user-prompt",
                "# AGENTS.md instructions for /repo\n\nrules",
                "2026-10-12T01:07:00Z",
            ),
        ];
        let window = select_window(&rows, false).unwrap();
        assert_eq!(prompts(&window), ["real"]);
    }

    #[test]
    fn limit_hit_without_start_marks_truncated() {
        let rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("user-prompt", "p", "2026-10-12T01:00:57Z"),
        ];
        assert!(select_window(&rows, true).unwrap().truncated);
        let recap = compose_recap(
            "sid",
            &select_window(&rows, true).unwrap(),
            &[],
            RECAP_BUDGET_DEFAULT,
        );
        assert!(recap.contains("not read"), "{recap}");
    }

    #[test]
    fn long_prompt_lands_intact_with_short_ones() {
        let goal = format!("/goal {}", "g".repeat(3_988));
        let rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("stop", &"r".repeat(1_994), "2026-10-12T02:01:05Z"),
            row("user-prompt", &"s".repeat(40), "2026-10-12T01:30:00Z"),
            row("user-prompt", &"m".repeat(96), "2026-10-12T01:07:07Z"),
            row("user-prompt", &goal, "2026-10-12T01:00:57Z"),
        ];
        let window = select_window(&rows, false).unwrap();
        let recap = compose_recap(
            "sid",
            &window,
            &[handoff("content session", true)],
            RECAP_BUDGET_DEFAULT,
        );
        assert!(recap.contains(&goal), "the /goal prompt was cut");
        assert!(recap.contains(&"m".repeat(96)));
        assert!(recap.contains(&"s".repeat(40)));
        assert!(recap.contains(&"r".repeat(1_000)));
        assert!(recap.contains("content session"));
        assert!(!recap.contains("omitted"), "{recap}");
        assert!(recap.chars().count() <= RECAP_BUDGET_DEFAULT);
    }

    #[test]
    fn overflow_drops_oldest_prompts_first() {
        let mut rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("stop", &"r".repeat(1_500), "2026-10-12T02:01:05Z"),
        ];
        for i in (0..10).rev() {
            rows.push(row(
                "user-prompt",
                &format!("PROMPT-{i:02} {}", "x".repeat(1_500)),
                &format!("2026-10-12T01:{i:02}:00Z"),
            ));
        }
        let window = select_window(&rows, false).unwrap();
        let recap = compose_recap("sid", &window, &[], RECAP_BUDGET_DEFAULT);
        assert!(recap.contains("PROMPT-09"), "newest prompt missing");
        assert!(!recap.contains("PROMPT-00"), "oldest prompt kept");
        assert!(recap.contains("earlier prompts omitted"), "{recap}");
        assert!(recap.contains(&"r".repeat(1_000)), "reply floor not held");
        assert!(recap.chars().count() <= RECAP_BUDGET_DEFAULT);
    }

    #[test]
    fn a_single_oversized_prompt_is_clipped_not_dropped() {
        let rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row(
                "user-prompt",
                &format!("HEAD {}", "y".repeat(20_000)),
                "2026-10-12T01:00:57Z",
            ),
        ];
        let window = select_window(&rows, false).unwrap();
        let recap = compose_recap("sid", &window, &[], RECAP_BUDGET_DEFAULT);
        assert!(recap.contains("HEAD "), "{recap}");
        assert!(recap.contains("[truncated]"), "{recap}");
        assert!(recap.chars().count() <= RECAP_BUDGET_DEFAULT);
    }

    #[test]
    fn missing_replies_say_how_to_capture_them() {
        let rows = vec![
            row("pre-compact", "", "2026-10-12T02:55:02Z"),
            row("user-prompt", "p", "2026-10-12T01:00:57Z"),
        ];
        let window = select_window(&rows, false).unwrap();
        let recap = compose_recap("sid", &window, &[], RECAP_BUDGET_DEFAULT);
        assert!(recap.contains("--capture-assistant"), "{recap}");
    }

    #[test]
    fn own_handoff_listed_first() {
        let window = select_window(&two_boundaries(), false).unwrap();
        let recap = compose_recap(
            "sid",
            &window,
            &[handoff("OTHER-BATON", false), handoff("OWN-BATON", true)],
            RECAP_BUDGET_DEFAULT,
        );
        let own = recap.find("OWN-BATON").unwrap();
        let other = recap.find("OTHER-BATON").unwrap();
        assert!(own < other, "{recap}");
    }

    #[test]
    fn recap_is_marked_untrusted_and_points_at_the_full_record() {
        let window = select_window(&two_boundaries(), false).unwrap();
        let recap = compose_recap("sid-1", &window, &[], RECAP_BUDGET_DEFAULT);
        assert!(recap.contains("compaction recap"));
        assert!(recap.contains(ai_memory_core::UNTRUSTED_MEMORY_NOTICE));
        assert!(recap.contains("<!-- ai-memory:untrusted-history:start -->"));
        assert!(recap.contains("<!-- ai-memory:untrusted-history:end -->"));
        assert!(recap.contains("memory_read_session_observations"));
        assert!(recap.contains("sessions/sid-1.md"));
        assert!(recap.contains("2026-10-12 01:00 UTC"), "{recap}");
    }

    #[test]
    fn notices_name_the_session() {
        assert!(no_compaction_notice("sid-1").contains("sid-1"));
        assert!(no_compaction_notice("sid-1").contains("no compaction"));
        assert!(no_session_notice().contains("no session"));
    }

    #[test]
    fn clamp_budget_bounds() {
        assert_eq!(clamp_budget(None), 8_000);
        assert_eq!(clamp_budget(Some("6000")), 6_000);
        assert_eq!(clamp_budget(Some("1")), 2_000);
        assert_eq!(clamp_budget(Some("99999")), 9_500);
        assert_eq!(clamp_budget(Some("x")), 8_000);
    }
}
