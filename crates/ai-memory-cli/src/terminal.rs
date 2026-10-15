//! A shared terminal vocabulary: quiet structure, semantic color and bounded motion.

mod progress;

use std::io::IsTerminal as _;

use crate::config::RuntimeEnv;
use crossterm::style::{Color, Stylize as _};

#[derive(Clone, Copy)]
pub(crate) enum Tone {
    Accent,
    Good,
    Warn,
    Bad,
    Muted,
}

pub(crate) struct Terminal {
    color: bool,
    motion: bool,
    width: usize,
    home: Option<String>,
}

impl Terminal {
    pub(crate) fn stdout(env: &RuntimeEnv) -> Self {
        let mut terminal = Self::new(
            std::io::stdout().is_terminal(),
            env.var_os("NO_COLOR").is_some(),
            env.var_os("TERM").is_some_and(|term| term == "dumb"),
        );
        terminal.home = env
            .var_os("AI_MEMORY_HOME")
            .or_else(|| env.var_os("HOME"))
            .map(|home| home.to_string_lossy().into_owned());
        terminal.motion = terminal.color && env.var_os("CI").is_none();
        if terminal.color
            && let Ok((width, _)) = crossterm::terminal::size()
        {
            terminal.width = usize::from(width).clamp(24, 80);
        }
        terminal
    }

    fn new(tty: bool, no_color: bool, dumb: bool) -> Self {
        Self {
            color: tty && !no_color && !dumb,
            motion: tty && !no_color && !dumb,
            width: 80,
            home: None,
        }
    }

    pub(crate) fn ink(&self, text: &str, tone: Tone) -> String {
        if !self.color {
            return text.to_owned();
        }
        let color = match tone {
            Tone::Accent => Color::Cyan,
            Tone::Good => Color::Green,
            Tone::Warn => Color::Yellow,
            Tone::Bad => Color::Red,
            Tone::Muted => Color::Grey,
        };
        match tone {
            Tone::Muted => text.with(color).to_string(),
            _ => text.with(color).bold().to_string(),
        }
    }

    pub(crate) fn header(&self, command: &str, version: &str) -> String {
        format!(
            "\n  {}  {}\n  {}\n  {}\n\n",
            self.ink("◈ ai-memory", Tone::Accent),
            self.ink(&format!("v{version}"), Tone::Muted),
            wrap(command, self.width.saturating_sub(4).max(8)).join("\n  "),
            self.ink(
                &"─".repeat(self.width.saturating_sub(4).min(64)),
                Tone::Muted
            )
        )
    }

    pub(crate) fn section(&self, name: &str) -> String {
        format!("\n  {}\n", self.ink(&name.to_uppercase(), Tone::Accent))
    }

    pub(crate) fn field(&self, name: &str, value: &str) -> String {
        let short = self
            .home
            .as_ref()
            .and_then(|home| value.strip_prefix(home))
            .filter(|tail| tail.is_empty() || tail.starts_with('/'))
            .map(|tail| format!("~{tail}"));
        let value = short.as_deref().unwrap_or(value);
        let mut label = format!("{name:<14}");
        if name.chars().count() >= 14 {
            label.push(' ');
        }
        let indent = label.chars().count();
        let mut out = String::new();
        for (index, line) in wrap(value, self.width.saturating_sub(indent + 2).max(8))
            .iter()
            .enumerate()
        {
            out.push_str(&format!(
                "  {}{line}\n",
                if index == 0 {
                    self.ink(&label, Tone::Muted)
                } else {
                    " ".repeat(indent)
                }
            ));
        }
        out
    }

    pub(crate) fn notice(&self, tone: Tone, title: &str, body: &str) -> String {
        let titles = wrap(title, self.width.saturating_sub(6).max(8));
        let mut out = String::from("\n");
        for (index, title) in titles.iter().enumerate() {
            out.push_str(&format!(
                "  {} {}\n",
                self.ink(if index == 0 { "╭─" } else { "│ " }, tone),
                self.ink(title, tone)
            ));
        }
        for paragraph in body.lines() {
            for line in wrap(paragraph, self.width.saturating_sub(6).max(8)) {
                out.push_str(&format!("  {} {line}\n", self.ink("│", tone)));
            }
        }
        out.push_str(&format!("  {}\n", self.ink("╰─", tone)));
        out
    }

    pub(crate) fn metrics(&self, metrics: &[(&str, u64)]) -> String {
        if self.width < 56 {
            return metrics
                .iter()
                .map(|(name, value)| self.field(name, &value.to_string()))
                .collect();
        }
        let mut values = String::from("  ");
        let mut labels = String::from("  ");
        for (name, value) in metrics {
            values.push_str(&self.ink(&format!("{:<20}", value), Tone::Accent));
            labels.push_str(&self.ink(&format!("{name:<20}"), Tone::Muted));
        }
        format!("{values}\n{labels}\n")
    }

    /// Group existing diagnostics without discarding their fields or remedies.
    pub(crate) fn document(&self, text: &str) -> String {
        let mut out = String::new();
        for line in text.lines() {
            let content = line.trim();
            if let Some((key, value)) = content.split_once(':')
                && !value.trim().is_empty()
                && key.len() <= 18
                && !content.starts_with(['!', '⚠'])
            {
                out.push_str(&self.field(&format!("{key}:"), value.trim()));
            } else {
                if !self.color {
                    out.push_str(line);
                    out.push('\n');
                } else {
                    let indent = line.len() - line.trim_start().len();
                    let prefix = " ".repeat(indent);
                    for part in wrap(
                        line.trim_start(),
                        self.width.saturating_sub(indent + 2).max(8),
                    ) {
                        out.push_str(&self.paint(&format!("{prefix}{part}\n")));
                    }
                }
            }
        }
        out
    }

    pub(crate) fn paint(&self, text: &str) -> String {
        text.split_inclusive('\n')
            .map(|line| {
                let content = line.trim_end_matches('\n');
                let tone = match content.trim_start() {
                    s if s.starts_with('✓') => Some(Tone::Good),
                    s if s.starts_with('!')
                        || s.starts_with('⚠')
                        || s.starts_with("Action required") =>
                    {
                        Some(Tone::Warn)
                    }
                    s if s.starts_with('✗') || s.starts_with("problem:") => Some(Tone::Bad),
                    s if s.starts_with("ai-memory ") => Some(Tone::Accent),
                    _ => None,
                };
                tone.map_or_else(
                    || line.to_owned(),
                    |tone| {
                        format!(
                            "{}{}",
                            self.ink(content, tone),
                            if line.ends_with('\n') { "\n" } else { "" }
                        )
                    },
                )
            })
            .collect()
    }
}

/// Wrap without truncating paths, commands or recovery instructions.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for ch in text.chars() {
        current.push(ch);
        if current.chars().count() >= width {
            if let Some(space) = current.rfind(' ')
                && space > current.len() / 2
            {
                let remainder = current[space + 1..].to_owned();
                lines.push(current[..space].to_owned());
                current = remainder;
            } else {
                lines.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod tests;
