use super::*;

#[test]
fn styles_and_motion_are_opted_out_together() {
    for (tty, no_color, dumb) in [
        (false, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let term = Terminal::new(tty, no_color, dumb);
        let output = term.header("Setup", "3.0.0")
            + &term.notice(Tone::Warn, "Action required", "Run /hooks");
        assert!(!output.contains('\u{1b}'));
        assert!(!term.motion);
        assert!(output.contains("Run /hooks"));
    }
    assert!(
        Terminal::new(true, false, false)
            .header("Status", "3.0.0")
            .contains('\u{1b}')
    );
}

#[test]
fn narrow_layout_wraps_instead_of_losing_instructions() {
    let mut term = Terminal::new(false, false, false);
    term.width = 32;
    let output = term.field("Storage", "/long/path/with/essential/information")
        + &term.notice(
            Tone::Warn,
            "Pending",
            "Open Codex, run /hooks, and approve the hooks.",
        );
    assert!(
        output.lines().all(|line| line.chars().count() <= 32),
        "{output}"
    );
    assert!(output.contains("/hooks"));
    assert!(term.metrics(&[("sessions", 12)]).contains("12"));
}

#[tokio::test]
async fn plain_progress_returns_the_actual_result_without_controls() {
    let term = Terminal::new(false, false, false);
    assert_eq!(term.waiting("Checking", async { 42 }).await, 42);
}
