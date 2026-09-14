---
title: videre status
description: Is the library up to date, what should I run next, and is my watcher alive.
---

A `git status` for your library: one read-only command that answers "is
everything up to date, what changed, what should I run next and roughly how
long will it take, and is my watcher alive?" It does no work and changes
nothing.

```bash
videre status                          # the whole picture, human-readable
videre status --json                   # one JSON object, for scripts and agents
videre status --check                  # exit non-zero only when a run failed or crashed
videre --library ~/Photos status       # select a different library
```

## Coverage

Per stage that has a real done-vs-outstanding count: how much has been
processed, what remains, and the command that closes the gap.

```
Coverage (model google/siglip-base-patch16-224):
  embed      0 of 1 done, 1 outstanding (optional, can take hours)
  faces      0 of 1 done, 1 outstanding
  locations  0 of 1 done, 1 outstanding
  fix-dates  0 of 1 done, 1 outstanding
```

- **embed** and **classify** are marked optional: they are the long stages,
  and a library that never runs them is a valid choice, not a problem.
- **faces** counts files never tried for faces, not photos with no faces in
  them. A landscape that was scanned and found faceless is done.
- **locations** and **fix-dates** count photos with GPS but no place name, and
  photos whose file date disagrees with the camera date, respectively.
- Duplicates are deliberately absent: finding them is
  [`videre dedupe`](/commands/dedupe/)'s job, and deleting anything is always
  yours.

## Pipeline status

The last run of every tracked command, with its outcome:

| status | meaning |
|---|---|
| `success` | finished, whatever it did or did not find |
| `failed` | returned an error |
| `interrupted` | stopped part-way, usually Ctrl-C or a machine going to sleep |
| `crashed` | claimed to be running, but no live process holds its lock |
| `-` | has never executed against this database |

`crashed` is what a kill -9, a power loss, or an OOM kill looks like after the
fact. `interrupted` is a clean stop, not a problem: nothing is lost, and
rerunning resumes. Per-item errors (a few unreadable photos) do not make a run
`failed`; only the command itself erroring does.

## Watch

```
Watch: running (last cycle 2026-09-14 12:30:11)
```

`watch` reports whether a watcher is alive right now and when its last cycle
completed. A watcher that died hours ago shows as `not running` with a stale
last-cycle time, which is the signal something went wrong: without a watcher,
the library silently stops staying current.

## Next actions

For every stage with outstanding work: the command to run, how many items it
would process, and an approximate duration.

```
Next actions:
  run 'videre embed': 12,431 item(s), ~1.6h
  run 'videre faces': 342 item(s), ~6m
```

Durations come from your own last measured run of that stage where one exists,
and from a coarse per-stage constant otherwise. They are marked approximate in
the JSON and rounded in the text; treat them as planning numbers, not
promises.

## `--check` for unattended runs

`--check` adds an exit code without changing any output: non-zero when a
tracked command's last run `failed` or `crashed`. Staleness never fails
`--check`, so a library mid-setup is healthy. Use it where cron or launchd can
act on the exit code:

```bash
# crontab: nightly refresh, alert only on real failures
0 3 * * * videre --library ~/Photos scan --retry-incomplete --silent && videre status --check
```

A cleanly interrupted run (Ctrl-C) never triggers alerts.
