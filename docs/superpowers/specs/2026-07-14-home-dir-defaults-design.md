# `~/.videre` Home Directory and Default Database

**Goal:** Stop passing `<db>` to every subcommand. videre gets a home directory
(`~/.videre/`) holding the default SQLite library, default JSONL output, and a small config
file; every command resolves its database automatically unless an explicit path is given.
`videre dedupe ~/Photos` followed by bare `videre report` / `videre search "sunset"` just
works.

**Non-goals (this slice):** Moving the thumbnail cache (stays at `~/.cache/videre/`), moving
model caches (`~/.cache/ort/`, Hugging Face hub), log files, storing report HTML under the
home dir, per-command option defaults in config (config holds exactly one key for now), any
remember-last-used-db behavior (explicitly rejected: explicit paths are one-invocation only
and never rewrite config).

**Sequencing:** This slice implements BEFORE the MCP server
(`2026-07-14-mcp-server-design.md`); that spec is amended so `videre mcp` takes an optional
`--db` resolved the same way.

---

## Layout

```
~/.videre/
  hashes.db       default SQLite library (created by writers on first use)
  hashes.jsonl    default JSONL output (only written when --output is used)
  config.toml     optional; currently holds only default_db
```

- The `VIDERE_HOME` environment variable overrides `~/.videre` entirely (used for test
  isolation; also a power-user escape hatch). All paths above are relative to it.
- The directory is created lazily by writers (dedupe, watch, `config set`); readers never
  create it.
- Future slices may add `cache/`, `models/`, `logs/`, report output, and more config keys;
  nothing in this slice forecloses that.

## Database resolution

One rule, used by every command:

1. **Explicit CLI path** (`--db` on readers, `--output-sqlite` on writers): used as-is,
   one invocation only, never saved anywhere.
2. **`default_db` from `$VIDERE_HOME/config.toml`**, when set.
3. **`$VIDERE_HOME/hashes.db`** otherwise.

Readers (report, search, fix-dates, prune, embed, faces, and later mcp) require the resolved
db to exist. If it does not, they print one friendly stderr line and exit 1:

```
no database found at /Users/you/.videre/hashes.db; run 'videre dedupe <dir>' first
```

(For `search --json`, this failure follows the existing error contract: the message is
emitted as the JSON error object on stdout instead.) This check replaces SQLite's
create-on-open behavior for defaulted paths; an explicitly passed `--db` keeps today's
per-command semantics unchanged.

Writers (dedupe, watch) create `$VIDERE_HOME` (with parents) when the resolved path is the
defaulted one; an explicit `--output-sqlite` path behaves exactly as today.

## CLI changes

Three deliberate breaking changes (pre-1.0, ergonomics redesign approved by the user):

1. **`db` positional becomes a `--db <path>` flag** on the six reader commands: `report`,
   `fix-dates`, `prune`, `embed`, `search`, `faces`. Reason: with an optional positional,
   `videre search "sunset"` cannot be disambiguated (db or query?). `--db` is uniform,
   unambiguous, and optional everywhere. All other flags on these commands are unchanged.
2. **Bare `videre dedupe <dir>` now writes SQLite to the resolved default db** (upsert, same
   as `--output-sqlite` today) instead of JSONL to `/tmp/hashes`. JSONL is written only when
   `--output` is passed:
   - `--output` with a value: JSONL appended to that path (as today).
   - `--output` bare (no value): JSONL appended to `$VIDERE_HOME/hashes.jsonl`. Implemented
     with a clap optional-value arg (`num_args(0..=1)`, i.e. `Option<Option<PathBuf>>`);
     the bare form resolves in code because the default depends on `VIDERE_HOME` at runtime.
   - `--output` and `--output-sqlite` remain mutually exclusive (conflict error, as today).
   - `--output-sqlite <path>`: explicit SQLite target, as today.
3. **`watch --output-sqlite` becomes optional**, defaulting to the resolved db (watch is a
   writer: it creates the home dir and db if absent).

Everything else stays byte-identical: flags, stdout/stderr text, exit codes, `--json`
documents (whose content never included the db path), the report HTML default output
location (`<db>_report.html` next to whatever db was resolved), and all behavior when an
explicit path is given.

Example session, before and after:

```bash
# before                                        # after
videre dedupe --output-sqlite ~/p.db ~/Photos   videre dedupe ~/Photos
videre report ~/p.db                            videre report
videre search ~/p.db "sunset on beach"          videre search "sunset on beach"
videre faces ~/p.db                             videre faces
videre watch --output-sqlite ~/p.db ~/Photos    videre watch ~/Photos
```

## `videre config` subcommand (tenth)

```
videre config                    # show resolved home, db path, and config file contents
videre config set db <path>      # write default_db into config.toml (creates dir/file)
videre config unset db           # remove default_db from config.toml
```

- `set db` stores the path verbatim after converting to an absolute path (a relative path
  saved into config would silently depend on the future working directory). It does not
  require the db to exist yet (you may set it before the first scan).
- `unset db` on a missing key or missing file is a no-op (exit 0).
- The config file is rewritten by parsing into a generic TOML table, mutating the one key,
  and writing back, so unknown keys added by future slices (or by hand) are preserved.
- `config` (show) works even when nothing exists yet: it prints the resolved defaults and
  notes the file is absent.

### config.toml format

```toml
# ~/.videre/config.toml
default_db = "/work/project.db"
```

Only `default_db` is read in this slice. Unknown keys are ignored on read and preserved on
write. A malformed config file is a hard error on any command that consults it (silent
fallback would mask typos and surprise the user); readers report the parse error and exit 1.

## Placement

- **New module `videre_core::home`:** `videre_home() -> PathBuf` (`VIDERE_HOME` if set, else
  `$HOME/.videre`; an unset `HOME` is a hard error with a clear message),
  `resolve_db(explicit: Option<&Path>) -> anyhow::Result<PathBuf>` (precedence chain; returns
  Err on malformed config; existence policy is applied by callers since readers and writers
  differ), `default_jsonl() -> PathBuf`, and config load/save helpers (`load_config`,
  `set_default_db`, `unset_default_db`).
- **New dependency:** `toml` in `videre-core`. No other new dependencies.
- **Command changes:** each reader swaps `db: PathBuf` (positional) for
  `#[arg(long)] db: Option<PathBuf>` and calls the resolver + existence check; dedupe and
  watch adjust their output flags as described; new `commands/config.rs` implements the
  config subcommand following the established module pattern.
- Note `videre-core` does not depend on the `videre` lib, so `home` deals only in paths;
  the `FileRecord`-level helpers stay in the `videre` crate as today.

## Error handling

- Missing resolved db (readers): the friendly one-liner above, exit 1 (JSON error object
  under `--json`).
- Malformed `config.toml`: parse error with the file path, exit 1, on every command that
  reads config.
- `config set db` with an un-absolutizable path or unwritable home dir: error, exit 1.
- No command ever writes to config except `videre config set/unset`.

## Testing

All integration tests set `VIDERE_HOME` to a per-test temp dir, so the real `~/.videre` is
never touched.

- **Unit (`videre_core::home`):** resolution precedence (explicit beats config beats
  default), `VIDERE_HOME` override respected, config set/unset roundtrip preserves unknown
  keys, malformed TOML is an error, relative `set db` path is absolutized.
- **Integration:**
  - Bare `dedupe <dir>` creates `$VIDERE_HOME/hashes.db` and upserts records; no JSONL file
    appears.
  - `dedupe --output <dir>` (bare flag) writes `$VIDERE_HOME/hashes.jsonl` and no db.
  - Bare reader (e.g. `prune --dry-run`) against the populated default db works; against an
    empty `VIDERE_HOME` prints the friendly error and exits 1 (and `search --json` yields the
    JSON error object).
  - `config set db <path>` redirects subsequent bare commands; explicit `--db` wins over
    config and does not modify it (config file unchanged after the run).
  - `videre config` prints the resolved paths.
  - Existing integration tests: mechanically updated from positional db to `--db <path>`
    (spawn sites), everything else unchanged, all previously passing tests still pass.
- **Docs:** README and CLAUDE.md rewritten to lead with the bare (flagless) forms, document
  `~/.videre/`, `VIDERE_HOME`, the resolution order, `videre config`, and the three
  deliberate breaks; the MCP spec is amended in the same change (optional `--db`).
