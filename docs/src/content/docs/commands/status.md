---
title: videre status
description: Is the library up to date, what should I run next, and is my watcher alive.
---

A `git status` for your library: one read-only command that answers "is
everything up to date, what changed, what should I run next, and is my watcher
alive?" It does no work and changes nothing.

```bash
videre status                          # the whole picture, human-readable
videre status --json                   # one JSON object, for scripts and agents
videre status --check                  # exit non-zero only when a run failed, crashed or logged an error
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
- **locations** counts photos with GPS but no location-cluster assignment.
  Place names are separate metadata and do not determine whether clustering is
  complete. It also reports `clusters stale` when the GPS data changed since the
  last recompute (for example after a prune or dedupe removed geotagged photos),
  which the outstanding count alone cannot see; run
  [`videre locations`](/commands/locations/) to refresh them.
- **fix-dates** counts photos whose file date represents a different instant
  from the camera date. Equivalent timezone-offset representations count as
  the same date.
- **skipped as undecodable** appears on a stage when a file has failed to decode
  enough times that `embed` or `faces` has stopped attempting it (an unreadable
  file, or one that repeatedly times out). Such files are not counted as
  outstanding, since the command will not act on them; `embed --reprocess`
  retries them for embed, and `faces --reset` for faces.
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

## Recent problems

When the latest run of a command logged errors or warnings, `status` lists it,
read from the per-command logs in `.videre/logs/`:

```
Recent problems (latest run of each command, see .videre/logs/):
  watch      run 2026-09-23 14:05: 2 error(s) (scan 2), 14 warning(s)
             last: source_unavailable: videre watch: scan stage: could not read /Volumes/Photos/2019 after 5s
  embed      run 2026-09-22 09:12: 0 error(s), 3 warning(s)
```

Each entry shows when that run started, in local time. Only the latest run
counts, so a clean run clears the entry. For `watch` and
`pipeline`, the errors are broken down by the stage that raised them. Warnings
are usually skipped files; errors are failures. The section is absent when
every latest run was clean. `--json` carries the same data under `report.logs`.
See [Logging and error handling](/guides/logging-and-errors/) for reading the
full logs.

## Next actions

For every stage with outstanding work: the command to run and how many items
it would process.

```
Next actions:
  run 'videre embed': 12,431 item(s) (intensive)
  run 'videre faces': 342 item(s)
```

A duration appears only when videre has enough measured data from a prior run
to calculate one. It never substitutes a coarse per-stage estimate. Until a
duration can be measured, long-running optional stages are marked
`(intensive)` instead.

## `--check` for unattended runs

`--check` adds an exit code without changing any output: non-zero when a
tracked command's last run `failed` or `crashed`, or when the latest run of any
command logged an error (a stage that failed inside `watch`, for example, even
though watch itself kept running). Warnings, such as skipped files, and
staleness never fail `--check`, so a library mid-setup is healthy. Use it where
cron or launchd can act on the exit code:

```bash
# crontab: nightly refresh, alert only on real failures
0 3 * * * videre --library ~/Photos scan --silent && videre status --check
```

A cleanly interrupted run (Ctrl-C) never triggers alerts.
