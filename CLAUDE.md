@AGENTS.md

<!--
Claude Code 2.1.277 and later reads AGENTS.md when CLAUDE.md and
CLAUDE.local.md are absent. This file still wins when present, so the
import on the first line is what loads the canonical instruction file
into a Claude Code session that loads CLAUDE.md (older versions, and the
default when this file exists): https://code.claude.com/docs/en/memory#agents-md

A prose "read AGENTS.md" pointer loads nothing at session start; it asks the
agent to open the file, which leaves adherence to whether it does.

Do not duplicate project rules here. Update AGENTS.md instead. Add content
below the import only when it is specific to Claude Code and has no AGENTS.md
counterpart.
-->
