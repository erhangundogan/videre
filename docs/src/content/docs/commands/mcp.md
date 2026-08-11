---
title: videre mcp
description: Expose read-only search, duplicate review, and stats to an AI assistant over stdio.
---

Serves read-only tools to an AI assistant using the Model Context Protocol. No
server to run, no port to open, nothing listening: the client starts videre as a
child process and talks to it over stdin and stdout.

```bash
videre mcp                             # serve using the default database
videre mcp --db ~/photos.db            # serve a specific database
videre mcp --model <model-id>          # serve searches from a specific model
```

You do not usually run this yourself. Your client runs it for you, using one of
the configurations below.

## The three tools

| Tool | Parameters | What it does |
|---|---|---|
| `stats` | none | Library summary: files, size, embeddings, faces, people, GPS coverage, date range |
| `find_duplicates` | `include_similar` | Exact-duplicate groups as `keep` and `remove`, plus review-only look-alikes |
| `search` | `query`, `person`, `image_path`, `top_k` | Semantic, person, or by-example search |

`search` takes exactly one of `query`, `person` or `image_path`. Text and image
search need [`videre embed`](/commands/embed/); `person` needs
[`videre faces`](/commands/faces/) plus naming.

`--category` and `--location` search are CLI-only. `--location` in particular is
excluded because it is the one mode that reaches the network and writes to the
database.

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
      "args": ["mcp"]
    }
  }
}
```

Restart Claude Desktop fully after editing. The tools appear once it reconnects.

## Claude Code

```bash
claude mcp add videre -- videre mcp
```

The `--` separates Claude Code's own flags from the command it should run. To
share the configuration with a repository instead of just your machine:

```bash
claude mcp add -s project videre -- videre mcp
```

That writes a `.mcp.json` in the project root, which you can also create by
hand:

```json
{
  "mcpServers": {
    "videre": {
      "command": "videre",
      "args": ["mcp"]
    }
  }
}
```

## Cursor

`~/.cursor/mcp.json` for every project, or `.cursor/mcp.json` inside one:

```json
{
  "mcpServers": {
    "videre": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["mcp"]
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
      "args": ["mcp"]
    }
  }
}
```

## Other clients

Almost every client uses the same three fields, differing only in the wrapper
key. If yours is not listed, look for where it keeps MCP servers and provide:

- **command**: the absolute path to `videre`
- **args**: `["mcp"]`
- **env**: optional, see below

Check your client's documentation for the exact key, since it is `mcpServers`
in most and `servers` in VS Code.

## Pointing at a specific library

Add arguments the same way you would on the command line:

```json
{
  "mcpServers": {
    "work-photos": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["mcp", "--db", "/Users/you/work.db"]
    }
  }
}
```

Or set a whole different home, which also switches config and caches:

```json
{
  "mcpServers": {
    "videre": {
      "command": "/opt/homebrew/bin/videre",
      "args": ["mcp"],
      "env": { "VIDERE_HOME": "/Users/you/videre-work" }
    }
  }
}
```

Nothing stops you registering several, one per library, under different names.
See [keeping libraries separate](/guides/multiple-libraries/).

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
videre mcp: serving /Users/you/.videre/hashes.db (model google/siglip-base-patch16-224)
{"jsonrpc":"2.0","id":1,"result":{...,"serverInfo":{"name":"videre","version":"0.11.4"}}}
```

That first line is on **stderr**, so it never corrupts the protocol stream, and
it is the quickest way to confirm which library got resolved.

If instead you see `no database found at ...` and it exits, the problem is the
library it resolved, not the client.

## Caveats

**The database must already exist.** Unlike other commands, `mcp` binds the
resolved path once at startup, so even an explicit `--db` pointing at a missing
file fails immediately with `no database found` on stderr and exit 1. Most
clients report this only as "server failed to start", which is why the manual
check above is useful.

**Results are as fresh as your last scan.** The tools read the database, not
your disk. Run [`videre watch`](/commands/watch/) to keep it current, and treat
paths as needing verification before anything acts on them.

**It is read-only.** Nothing exposed here writes to the database or touches your
files. An assistant can find duplicates but cannot delete them; you run
[`videre dedupe`](/commands/dedupe/) yourself.

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
