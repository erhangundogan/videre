---
title: Logging and error handling
description: Where videre records failures, how to read the log files, and how to control what they keep.
---

Errors printed to the terminal scroll away, especially under a long
[`videre watch`](/commands/watch/) or [`videre pipeline`](/commands/pipeline/).
videre also writes them to per-command log files inside the library, so you can
answer "what went wrong" afterwards.

## Where the logs are

```
<library>/.videre/logs/
  scan.log            # errors and warnings from `videre scan`
  search.log          # errors and warnings from `videre search`
  scan.trace.log      # only when log-level is info or debug
  scan.log.1          # an older, rotated file
```

Each file belongs to the command you ran. Every run writes one `run started`
line to it, then its errors and warnings: a run with nothing but that line went
cleanly, and it supersedes an older failed run when
[`videre status`](/commands/status/) reads the latest one.
`<command>.log` always holds that command's errors and warnings, whatever
`log-level` says.
`<command>.trace.log` exists only when you raise
`log-level` to `info` or `debug`, and holds every line at that level, errors
included, so the detail around a failure stays in one place. Detailed logging
can therefore never push an error out of the primary file.

What is recorded:

- **A command's failure**, once, with the same text the terminal shows after
  `error:`. When a command reports its failure as a JSON document on stdout
  (`--json`), the failure is still logged but not repeated on stderr, so the
  JSON output stays clean.
- **Every file that was skipped**, as a warning naming the file in
  `fields.path`: an unreadable file during `scan`, an image `embed` or `faces`
  could not decode.
- **A failed stage of `pipeline` or `watch`**, which carries on to the next
  stage. It is recorded in the file of the command you ran, with the stage in
  `fields.stage`: a scan failure inside `videre watch` is a line in
  `watch.log` with `"stage":"scan"`, never a line in `scan.log`.
- **A failed request in `videre gallery`** (anything that returned a server
  error) and **a failed tool call in `videre mcp`**.

You rarely need to open the files first:
[`videre status`](/commands/status/) shows the latest run of every command that
logged errors or warnings, with its last error, and `videre status --check`
exits non-zero when a latest run logged an error.

## Choosing what is kept

```bash
videre config set log-level debug     # error, warn (default), info, debug
videre config set log-format text     # json (default) or text
videre config set log-max-size-mb 20  # rotate at this size (default 10)
videre config set log-keep 3          # rotated files kept per file (default 5)
videre config set log-max-age-days 7  # delete rotated files older than this (default 30)
```

The settings belong to the library, in its `.videre/config.toml`. They control
the files only: what the terminal shows never changes. `log-level` decides the
trace file: `info` and `debug` create it, while `error` and `warn` leave only
the primary file, which always keeps both errors and warnings.

`info` adds everything a command prints while it works (progress summaries,
"Loading model", "wrote N record(s)") to the trace file. `debug` adds the
decisions that are otherwise invisible: the log settings in force, every lock
taken or refused, how long each `pipeline` and `watch` stage took, files
skipped after repeated decode failures, and thumbnail cache hits and misses.
Use it when reporting a problem; it grows quickly.

When a file reaches `log-max-size-mb` it is renamed to `.1` (the previous `.1`
becomes `.2`, and so on) and a new file starts. At most `log-keep` rotated files
are kept, and rotated files older than `log-max-age-days` are deleted the next
time the command runs. The active file is never deleted.

## Reading the files

The default format is JSON Lines, one object per line, as written by the Rust
`tracing` library:

```json
{"timestamp":"2026-09-23T13:01:15.109720Z","level":"ERROR","fields":{"message":"no embeddings for google/siglip-base-patch16-224 in this library\n  run: videre embed","kind":"","remediation":"","path":"","stage":""},"target":"videre_core::error_log","spans":[{"command":"search","run":"20260923T130115Z-58217","name":"run"}]}
```

- `timestamp` is UTC.
- `spans[0].command` is the command you ran, and `spans[0].run` identifies one
  invocation (its start time and process id), so lines from the same run can be
  grouped.
- `fields.message` is the full error chain, exactly as the terminal showed it;
  `fields.kind`, `fields.remediation`, `fields.path` and `fields.stage` are
  filled in when videre knows them, and empty otherwise.

`jq` answers most questions:

```bash
cd ~/Photos
# every error
jq -c 'select(.level=="ERROR")' .videre/logs/scan.log
# just the messages
jq -r '.fields.message' .videre/logs/scan.log
# the lines of the most recent run
jq -s 'group_by(.spans[0].run) | last' .videre/logs/scan.log
```

With `log-format text`, each line is [logfmt](https://brandur.org/logfmt)
instead, readable with `less` or `grep`:

```text
ts=2026-09-23T13:01:15.151303Z level=error target=videre_core::error_log span=run message="no embeddings for google/siglip-base-patch16-224 in this library\n  run: videre embed" kind= remediation= path= stage= command=search run=20260923T130115Z-58220
```

Changing the format affects only new lines; older lines in the same file stay
readable.

## Error kinds

When videre knows why something failed, the line carries a `kind` and a
`remediation`. The kinds are few on purpose; most failures are environmental.

| `kind` | Meaning | What to do |
|---|---|---|
| `source_unavailable` | A file is missing, or it or the library could not be read from its drive (a timeout or a device error) | Reconnect the drive holding the library, then run the command again |
| `permission_denied` | The operating system refused access | Grant read access to the file or folder |
| `decode_failed` | The file could not be decoded as an image or video frame | The file is unreadable or unsupported; videre skips it after two attempts |
| `quicklook_unavailable` | HEIC and video need macOS QuickLook | These files are skipped on this platform |
| `model_unavailable` | A search or face model could not be downloaded or loaded | Check network access for the first run, or the Hugging Face cache location |
| `library_busy` | Another videre command holds the library or this command's lock | Retry when it finishes; `watch` retries by itself |
| `library_schema` | The database needs an upgrade, or was written by a newer videre | Follow the message: run a writer command such as `videre scan`, or upgrade videre |
| `database` | SQLite reported an error | The message says which operation failed |

A failure without a known cause has an empty `kind`; its message is still the
full error.

```bash
# the failures that have a known cause, grouped
jq -r 'select(.fields.kind != "") | .fields.kind' .videre/logs/watch.log | sort | uniq -c
```

## Privacy

Log lines contain paths to your photos and videos. The files are created
readable by your user only (mode `0600`, in a `0700` directory), are never sent
anywhere, and can be deleted at any time: remove `.videre/logs/` and videre
starts fresh on the next run.

## When nothing is written

Logging never changes what a command does. It is skipped, with at most one
warning on the terminal, when:

- the library has not been initialized yet (no `.videre/`): reading a library
  never creates state;
- `.videre/logs/` or a log file is a symlink, which videre refuses for all of
  its state;
- the volume is read-only or full, or the file cannot be opened.

On a slow or disconnected drive, log lines are written in the background and
dropped rather than waited for, so logging cannot hang a command. If any were
dropped, videre says how many when the command ends.
