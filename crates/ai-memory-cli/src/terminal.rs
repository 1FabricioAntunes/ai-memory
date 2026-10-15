//! Explicit formatting for human diagnostic reports. Never intercepts output.

use std::fmt::Arguments;
use std::io::IsTerminal as _;

use crossterm::style::{Color, Stylize as _};

use crate::config::RuntimeEnv;

pub(crate) struct Terminal {
    color: bool,
}

impl Terminal {
    pub(crate) fn stdout(env: &RuntimeEnv) -> Self {
        Self::new(std::io::stdout().is_terminal(), env)
    }

    fn new(tty: bool, env: &RuntimeEnv) -> Self {
        Self {
            color: tty
                && env.var_os("NO_COLOR").is_none()
                && env.var_os("TERM").is_none_or(|term| term != "dumb"),
        }
    }

    pub(crate) fn header(&self, command: &str) {
        if self.color {
            println!(
                "\n  {}\n  {}\n  {}\n",
                "◈ ai-memory".cyan().bold(),
                command,
                "────────────────────────────────────────────────".dark_grey(),
            );
        }
    }

    pub(crate) fn section(&self, title: &str) {
        if self.color {
            println!("\n  {}", title.to_uppercase().cyan().bold());
        }
    }

    pub(crate) fn line(&self, args: Arguments<'_>) {
        println!("{}", self.paint(&args.to_string()));
    }

    pub(crate) fn text(&self, text: &str) {
        print!("{}", self.paint(text));
    }

    /// Decoration never drops, reorders, wraps or truncates report content.
    fn paint(&self, text: &str) -> String {
        if !self.color {
            return text.to_owned();
        }
        text.split_inclusive('\n')
            .map(|line| {
                let value = line.trim_end_matches('\n');
                let trimmed = value.trim_start();
                let color = if trimmed.starts_with(['⚠', '!', '?']) {
                    Some(Color::Yellow)
                } else if trimmed.starts_with('✓') {
                    Some(Color::Green)
                } else if ["pages:", "sessions:", "observations:"]
                    .iter()
                    .any(|key| trimmed.starts_with(key))
                {
                    Some(Color::Cyan)
                } else {
                    None
                };
                let decorated = if let Some(color) = color {
                    value.with(color).bold().to_string()
                } else if value.starts_with(' ')
                    && let Some((label, rest)) = value.split_once(':')
                    && label.trim().len() <= 22
                {
                    format!("{}:{rest}", label.dark_grey())
                } else {
                    value.to_owned()
                };
                format!(
                    "{decorated}{}",
                    if line.ends_with('\n') { "\n" } else { "" }
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_no_color_and_dumb_terminals_keep_exact_report_text() {
        let text = "  pages: 12\n⚠ Review hooks\n    ai-memory install-hooks --agent codex --apply\n  db: C:\\memory dir\\db.sqlite\n";
        for (tty, pairs) in [
            (false, vec![]),
            (true, vec![("NO_COLOR", "")]),
            (true, vec![("NO_COLOR", "1")]),
            (true, vec![("TERM", "dumb")]),
        ] {
            let terminal = Terminal::new(tty, &RuntimeEnv::with_process_env_for_tests(&pairs));
            assert!(!terminal.color);
            assert_eq!(terminal.paint(text), text);
        }
    }

    #[test]
    fn styled_reports_preserve_every_character_including_unknown_fields() {
        let terminal = Terminal::new(
            true,
            &RuntimeEnv::with_process_env_for_tests(&[("TERM", "xterm-256color")]),
        );
        let text = "  pages: 42\n  future field: /a/path with spaces\n✓ Agent captured\n⚠ Unknown warning; keep this remedy\n? Capture unknown\n";
        let decorated = terminal.paint(text);
        assert!(decorated.contains('\u{1b}'));
        let mut plain = String::new();
        let mut chars = decorated.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{1b}' {
                for code in chars.by_ref() {
                    if code == 'm' {
                        break;
                    }
                }
            } else {
                plain.push(ch);
            }
        }
        assert_eq!(plain, text);
    }
}
