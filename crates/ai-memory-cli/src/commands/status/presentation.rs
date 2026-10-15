//! Runtime overview; complete counters remain available through status --json.

use super::*;
use crate::terminal::{Terminal, Tone};

pub(super) fn render(
    terminal: &Terminal,
    report: &Report,
    spool: &SpoolHealth,
    ep: &ServerEndpoint,
    details: &str,
) -> String {
    let mut out = terminal.header("STATUS / memory & capture", &report.version);
    out.push_str(&terminal.field("Server", &ep.url));
    out.push_str(&terminal.metrics(&[
        ("pages", report.counts.pages_latest),
        ("sessions", report.counts.sessions),
        ("observations", report.counts.observations),
    ]));
    if spool.evicting()
        || report
            .ingest
            .as_ref()
            .is_some_and(|ingest| ingest.failed > 0)
    {
        out.push_str(&terminal.notice(Tone::Warn,"CAPTURE NEEDS ATTENTION", "Review the queue and ingestion counters below.\nRecorded sessions alone do not verify current hook delivery."));
    } else if spool.pending > 0 {
        out.push_str(&terminal.notice(
            Tone::Warn,
            "EVENTS WAITING FOR DELIVERY",
            &format!(
                "{} events are queued locally. Review delivery below.",
                spool.pending
            ),
        ));
    } else if report.counts.sessions == 0 && report.counts.observations == 0 {
        out.push_str(&terminal.notice(Tone::Accent,"WAITING FOR YOUR FIRST SESSION", "Start your agent in a project, then run ai-memory doctor there.\nThe server is responding; capture has not been verified."));
    } else {
        out.push_str(&terminal.field(
            "Capture",
            "Stored sessions found · check doctor for current hook delivery",
        ));
    }
    if report.counts.pages_all > report.counts.pages_latest {
        out.push_str(&terminal.field("Page versions", &report.counts.pages_all.to_string()));
    }
    let mut section = "";
    let quiet_delivery = spool.pending == 0
        && !details.contains("  recovery:")
        && report.ingest.as_ref().is_none_or(|ingest| {
            ingest.accepted == 0
                && ingest.failed == 0
                && ingest.dropped_by_policy == 0
                && ingest.dropped_unauthorized == 0
                && ingest.dropped_collision == 0
                && ingest.dropped_invalid == 0
                && ingest.shed_saturated == 0
                && ingest.shed_rate_limited == 0
        });
    for line in details.lines().skip(1) {
        let key = line.trim().split(':').next().unwrap_or("");
        if [
            "server",
            "db",
            "bind",
            "filesystem free",
            "wiki format",
            "providers",
        ]
        .contains(&key)
        {
            continue;
        }
        if key == "embeddings"
            && report.derived.embedding_rows == 0
            && report.derived.latest_pages_missing_embeddings == 0
        {
            continue;
        }
        if key == "links"
            && report.derived.links_from_latest_pages == 0
            && report.derived.unresolved_links_from_latest_pages == 0
            && report.derived.stale_links_from_latest_pages == 0
        {
            continue;
        }
        if quiet_delivery
            && [
                "spool",
                "pending",
                "oldest",
                "retries",
                "ingest (server, this process)",
                "accepted",
                "dropped",
                "outcomes",
                "drops",
                "shed",
                "last write",
            ]
            .contains(&key)
        {
            continue;
        }
        let next = match key {
            "server" | "data-dir" | "db" | "bind" | "auth" => "CONNECTION",
            "pages" | "sessions" | "observations" => continue,
            "fts" | "embeddings" => "SEARCH INDEX",
            "storage" | "links" | "wiki format" => "STORAGE & LINKS",
            "write queue"
            | "spool"
            | "recovery"
            | "ingest (server, this process)"
            | "capture mode" => "CAPTURE & DELIVERY",
            "providers" | "llm" | "embedding" => "OPTIONAL MODELS",
            _ => section,
        };
        if next != section {
            out.push_str(&terminal.section(next));
            section = next;
        }
        out.push_str(&terminal.document(line));
    }
    out.push_str(&terminal.section("Inspect"));
    out.push_str(&terminal.field("Full details", "ai-memory status --json"));
    out
}
