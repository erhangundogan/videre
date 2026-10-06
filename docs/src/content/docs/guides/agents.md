---
title: Working with AI agents
description: videre is built to be driven by an assistant as well as by you, through its command line or over MCP.
---

An AI agent works by running a step, reading what came back, and deciding the
next one. videre is built for that loop. Every command can be scripted end to
end, answers in JSON when asked, says exactly what it would change before it
changes it, and refuses rather than guesses. So an assistant can manage a
library of tens of thousands of photos with you watching, not typing.

## Two ways in

| | The command line | [MCP](/commands/mcp/) |
|---|---|---|
| For | Agents with a shell: Claude Code, Cursor's agent, Copilot's agent mode, Codex CLI, Gemini CLI | Any MCP client, including chat apps with no shell, such as Claude Desktop's chat |
| Can | Everything: scan, search, dedupe, tag, mark, rotate, export | Search, duplicates, stats: reading only |
| Supervision | You approve the commands that change things | None needed: nothing it can call changes your files |

Both run the same code, so a search returns the same files either way.

## What makes the command line agent-ready

**Structured output.** `--json` prints one JSON document on stdout, and only
that: progress and summaries go to stderr. `search`, `stats`, `status`, `scan`,
`import`, `faces`, `locations`, `pipeline` and every `dedupe` action take it.
On a failure the document is an error object and the exit code is nonzero, so
an agent never has to parse a sentence to learn what went wrong. `export
--jsonl` streams one record per line for larger sets.

**Look before you leap.** The commands that move, delete or rewrite your
files, and most that reshape the library, take `--dry-run`, which lists what
they would do and does nothing: `import`, `fix-dates`, `prune`, `faces`,
`mark`, `export`, `pipeline`, and `dedupe trash`, `delete` and `undo`. `--yes` then runs it without an interactive prompt. The
natural loop is: dry run, show you the list, run it once you agree.

**One way to say "which files".** The [query language](/reference/query-syntax/)
works the same in `search`, the gallery, and the `--query` of `dedupe`, `embed`,
`faces`, `classify`, `mark`, `tag` and `export`:

```bash
videre search 'person:özgür date:2023 tag:tatil'
videre tag --query 'place:kadıköy date:2024' --add istanbul
videre dedupe trash --kind resized --query 'path:WhatsApp' --dry-run
```

An agent that learns it once can scope any command. Every scoped run prints
`N of M`, so a filter that matched nothing is never mistaken for an empty
library.

**It refuses rather than guesses.** A second copy of a running command is
refused, not queued behind the first, so an agent never hangs waiting on a
lock it cannot see. A missing drive, an implausibly large deletion, or a query
with a typo stops the command with a message that says why, before anything is
touched.

**It says what to run next.** [`videre status`](/commands/status/) reports what
is outstanding and the command that finishes it, and errors carry their
remedy, such as `run: videre embed`. An agent can bring a library up to date by
following what videre tells it.

**Mistakes can be undone.** `dedupe trash` and the gallery's Delete move files
to the system trash and record what they moved, and
[`videre dedupe undo`](/commands/dedupe/#undoing-a-trash-run) puts the last run
back. An agent can act, and you can still change your mind.

**It describes itself.** `--help` on every command lists its flags and what
they do, and shell completion offers the values from your own library, such as
people, tags and models. An agent needs no hidden knowledge to use it.

**It leaves a record.** Each command logs its errors to
`.videre/logs/<command>.log`, one JSON object per line. When something fails,
an agent can read what happened instead of asking you to look; see
[logging and error handling](/guides/logging-and-errors/).

## Some things to ask

With a shell agent:

- "Bring my library up to date and tell me what changed."
- "Find the resized WhatsApp copies, show me the list, and trash them once I
  say yes."
- "Tag every photo from Kadıköy in 2024 as istanbul."
- "Show me the photos with both Ayşe and Özgür from last summer."

With MCP, in any chat:

- "How many photos do I have from 2015, and where were they taken?"
- "Find photos of the sea at sunset."
- "Which Google Photos creations could I remove?"

## Keeping control

An agent with a shell can do anything you can, so read what it proposes before
it changes your library. A few habits help:

- Ask for `--dry-run` first, and approve the real run yourself.
- Prefer `dedupe trash` to `dedupe delete`: trash can be undone, delete cannot.
- For questions alone, MCP is enough, and it cannot change anything.
- Keep a [backup](/guides/backup/) of anything you could not replace, as you
  would with any tool that deletes.
