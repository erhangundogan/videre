---
title: videre mcp
description: Expose read-only search, duplicate review, and stats to an AI agent over stdio.
---

Serves read-only tools to an AI assistant over stdio, using the Model Context
Protocol. No server to run, no port to open.

```bash
videre mcp                             # serve using the default database
videre mcp --db ~/photos.db            # serve a specific database
```

## Client configuration

```json
{
  "mcpServers": {
    "videre": {
      "command": "/path/to/videre",
      "args": ["mcp"]
    }
  }
}
```

## The three tools

| Tool | What it does |
|---|---|
| `search` | Text, person, and image search |
| `find_duplicates` | Keep/remove groups, plus look-alike groups via `include_similar` |
| `stats` | Library summary, no parameters |

`--category` and `--location` search are CLI-only and not exposed here.
`--location` in particular is excluded because it is the one mode that reaches
the network and writes to the database.

All three results share `"schema_version": 1` with the CLI's `--json` output and
reuse the same shapes.

## Behaviour

The database must already exist. Unlike other commands, `mcp` binds the resolved
path once at startup for the life of the process, so even an explicit `--db`
pointing at a nonexistent file fails immediately rather than creating one.

A failing tool call returns an error result; the server stays alive and keeps
serving.

The search model loads lazily on the first text or image search and stays in
memory, unlike the CLI which reloads it per invocation. Person search never
touches the model.
