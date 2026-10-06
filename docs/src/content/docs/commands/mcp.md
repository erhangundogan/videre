---
title: videre mcp
description: Expose read-only search, duplicate review, and stats to an AI assistant over stdio.
---

Serves read-only tools to an AI assistant using the Model Context Protocol. No
server to run, no port to open, nothing listening: the client starts videre as a
child process and talks to it over stdin and stdout.

```bash
videre --library ~/Photos mcp          # serve one library
videre mcp                             # serve the library in this directory
videre mcp --model <model-id>          # serve searches from a specific model
```

You do not usually run this yourself. Your client runs it for you, using one of
the configurations below, and stops it when it is done.

## Why MCP, when the CLI does everything?

It does: every MCP tool runs the same code as a command, so `search` returns
what [`videre search`](/commands/search/) returns and `find_duplicates` what
[`videre dedupe --json`](/commands/dedupe/) prints. An assistant that can run
shell commands, such as Claude Code, can use the CLI directly, and that works
well. MCP is for the other cases:

- **Assistants with no shell.** Claude Desktop, Cursor's chat and most chat
  apps cannot run `videre`. MCP is the only way they can reach your library.
- **Read-only by construction.** The server offers three tools that read, and
  nothing that trashes, deletes, rotates or tags. You can let an assistant
  search without approving each command, and it cannot remove a photo by
  mistake. (Searching by place name may look the name up online once and save
  the answer, the one write; see below.)
- **Self-describing.** Each tool arrives with its parameters and what they
  mean, so the assistant needs no knowledge of the CLI's flags, and always
  gets one structured JSON document back rather than terminal text to parse.

So with Claude Code, use whichever you prefer; elsewhere, or when you want
access you do not have to supervise, use MCP.

## The three tools

| Tool | Parameters | What it does |
|---|---|---|
| `stats` | none | Library summary: files, size, embeddings, faces, people, GPS coverage, date range |
| `find_duplicates` | `kinds` | Duplicate groups by [kind](/commands/dedupe/#kinds) (`exact` by default; `resized`, `creation`, `similar`): `keep` and `remove`, or a review-only `files` list. The document `dedupe --json` prints |
| `search` | `query`, `image_path`, `person`, `category`, `location`, `radius_km`, `after`, `before`, `date`, `sort`, `top_k` | Composed search: filters narrow, a ranker orders |

`search` takes **at most one ranker** (`query` or `image_path`) and any number of
filters (`person`, `category`, `location` + `radius_km`, and `after`/`before`/`date`),
which AND together. At least one of the two is required. `sort` accepts a
comma-separated `field[:asc|desc]` list.

Text and image search need [`videre embed`](/commands/embed/); `person` needs
[`videre faces`](/commands/faces/) plus naming; `category` needs
[`videre classify`](/commands/classify/).

The MCP server and the CLI run the identical code path, so a composed tool call
and the equivalent `videre search` invocation return the same results by
construction. See [compositional searches](/guides/compositional-search/) for
what composes with what.

:::caution[`location` is the one parameter that reaches the network]
Geocoding a place name makes an outbound request on a cache miss and writes the
answer to the `geocode_cache` table, so it is also the only part of this
read-only server that writes anything. Repeats are served locally.
:::

## Finding the binary path

:::caution[Use an absolute path]
This is the single most common setup problem. Desktop applications do not
inherit your shell's `PATH`, so a bare `videre` often fails with "command not
found" even though it works in your terminal.
:::

```bash
which videre
```

Typical results:

| Install | Path |
|---|---|
| Homebrew, Apple Silicon | `/opt/homebrew/bin/videre` |
| Homebrew, Intel or Linux | `/usr/local/bin/videre` or `/home/linuxbrew/.linuxbrew/bin/videre` |
| `cargo install` | `~/.cargo/bin/videre` |

Use that full path in the configs below. Command-line clients such as Claude
Code inherit your `PATH`, so a bare `videre` is fine there.

## Claude Desktop

Edit the config file, creating it if it does not exist:

- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

```json
{
  "mcpServers": {
    "videre": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["--library", "/Users/you/Photos", "mcp"]
    }
  }
}
```

Restart Claude Desktop fully after editing. The tools appear once it reconnects.

:::caution[Always name the library]
A client starts `videre mcp` from its own folder, not from your library, so
without `--library` it finds no library and the server fails to start. Running
`videre mcp` by hand inside the library folder works, which makes this easy to
miss. Every example here passes `--library`.
:::

## Claude Code

```bash
claude mcp add -s user videre -- videre --library ~/Photos mcp
```

The `--` separates Claude Code's own flags from the command it should run.
`-s user` makes the server available in every project; without it, it is added
for the current project only. To share the configuration with a repository
instead:

```bash
claude mcp add -s project videre -- videre --library ~/Photos mcp
```

That writes a `.mcp.json` in the project root, which you can also create by
hand:

```json
{
  "mcpServers": {
    "videre": {
      "command": "videre",
      "args": ["--library", "/Users/you/Photos", "mcp"]
    }
  }
}
```

`claude mcp list` checks every server and shows whether it connected; inside a
session, `/mcp` shows the same and can reconnect one. A server added or removed
appears in the next session, not in the one already running.

## Cursor

`~/.cursor/mcp.json` for every project, or `.cursor/mcp.json` inside one:

```json
{
  "mcpServers": {
    "videre": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["--library", "/Users/you/Photos", "mcp"]
    }
  }
}
```

## VS Code

`.mcp.json` in the workspace, or the user-level MCP settings. Note that VS Code
uses `servers` rather than `mcpServers`, and wants an explicit type:

```json
{
  "servers": {
    "videre": {
      "type": "stdio",
      "command": "/opt/homebrew/bin/videre",
      "args": ["--library", "/Users/you/Photos", "mcp"]
    }
  }
}
```

## Other clients

Almost every client uses the same three fields, differing only in the wrapper
key. If yours is not listed, look for where it keeps MCP servers and provide:

- **command**: the absolute path to `videre`
- **args**: `["--library", "<your library>", "mcp"]`
- **env**: optional, see below

Check your client's documentation for the exact key, since it is `mcpServers`
in most and `servers` in VS Code.

## More than one library

A server binds to one library at startup, chosen by `--library` (or, with no
argument, the directory the client starts it in), and serves it for its
lifetime: no tool switches libraries. For several libraries, register one
server per library, each under its own name:

```bash
claude mcp add -s user videre-photos -- videre --library ~/Photos mcp
claude mcp add -s user videre-takeout -- videre --library /Volumes/Archive/Takeout mcp
```

```json
{
  "mcpServers": {
    "videre-photos": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["--library", "/Users/you/Photos", "mcp"]
    },
    "videre-takeout": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["--library", "/Volumes/Archive/Takeout", "mcp"]
    }
  }
}
```

Each runs as its own process, and the assistant sees both sets of tools. Name
the library when you ask ("search my Takeout library for ..."), or it may pick
either or ask which you mean. See
[keeping libraries separate](/guides/multiple-libraries/).

## A library on an external drive

The server checks the library when it starts. With the drive unplugged it
cannot, so the server fails to start and the client shows it as failed;
nothing else is affected, and the assistant simply has no videre tools. Plug
the drive back in, then reconnect (`/mcp` in Claude Code, or restart the
client). There is no need to remove and add the server again. A drive that
goes away while the server is running makes its tool calls fail the same way
until then.

## Checking it works

You can drive the server by hand, which is the quickest way to tell a videre
problem from a client problem:

```bash
printf '%s\n' \
 '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}' \
 '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
 '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
 | videre mcp
```

A working server prints one line to stderr naming the database and model it is
serving, then replies on stdout with its name and version and lists the three
tools:

```
videre mcp: serving /Users/you/.videre/hashes.db (model google/siglip2-base-patch16-224)
{"jsonrpc":"2.0","id":1,"result":{...,"serverInfo":{"name":"videre","version":"0.11.4"}}}
```

That first line is on **stderr**, so it never corrupts the protocol stream, and
it is the quickest way to confirm which library got resolved.

If instead you see `no database found at ...` and it exits, the problem is the
library it resolved, not the client.

## Caveats

**The database must already exist.** Unlike other commands, `mcp` binds the
selected library once at startup, so a library with no database that is missing
file fails immediately with `no database found` on stderr and exit 1. Most
clients report this only as "server failed to start", which is why the manual
check above is useful.

**Results are as fresh as your last scan.** The tools read the database, not
your disk. Run [`videre watch`](/commands/watch/) to keep it current, and treat
paths as needing verification before anything acts on them.

**It is read-only.** Nothing exposed here touches your files, and the only
database write is the place-name cache above. An assistant can find duplicates
but cannot delete them; you run [`videre dedupe trash`](/commands/dedupe/)
yourself, or use the gallery's Duplicates page.

**The first text or image search is slow.** The embedding model loads on demand
and then stays in memory for the life of the process, so later searches are
fast. Person search never loads it.

**A failing tool call does not kill the server.** It returns an error result and
keeps serving.

**Restart the client after config changes.** Servers are started once at client
startup, so edits do not take effect until it reconnects.

## More detail

- [Keeping libraries separate](/guides/multiple-libraries/) covers serving more
  than one collection.
- [Long-running jobs](/guides/long-running-jobs/) covers running this alongside
  a `videre watch` that keeps the database fresh.
