# First-run checks

`ai-memory setup` checks installation, server connectivity and the memory API.
These checks do not verify that an agent is delivering session events.

- **Hook approval required:** open Codex, run `/hooks`, and approve the
  ai-memory hooks. Setup does not bypass this approval.
- **Restart required:** restart the selected agent to load its integration.
- **Capture not yet verified:** start a session in a project, then run
  `ai-memory doctor` from that project. Existing local sessions may predate
  installation; zero captured sessions alone does not identify the cause.
- **Installation incomplete:** resolve the reported problems and re-run setup.

Doctor checks the current Codex approval state even when the server contains
older captured sessions. Detected Codex and Claude Code hooks with no captures
get restart and delivery checks rather than a reinstallation instruction.
Inactive agents with no captures have a neutral status.

Setup and doctor use status colors on interactive terminals. `NO_COLOR` or
`TERM=dumb` disables colors. Redirected output and `doctor --json` stay plain.
Routine startup traces stay in the log file for setup, doctor and status;
`RUST_LOG=info` or `RUST_LOG=debug` also shows tracing on stderr. Installer
warnings and failures remain visible.

Setup groups changed files, backup notices and script counts by agent, combines
identical capture-policy messages with their agent names, and keeps token,
hook-trust, restart and finalization instructions in an agent next-steps section.
Standalone `install-mcp` and `install-hooks` keep their detailed output.

The installer, setup, status and doctor share section headings, status colors,
and a compact header. Status highlights memory counts and only expands quiet
queues when there is activity; `status --json` keeps all diagnostic fields.
Setup shows a single moving indicator while awaiting the service or network
checks. It never adds a delay or invents a completion percentage. Redirected
output, `NO_COLOR`, and `TERM=dumb` disable motion; CI also uses static progress.
