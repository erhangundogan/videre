---
title: videre dedupe
description: List duplicate copies you could delete, one path per line.
---

Finds duplicates already recorded in the database. Prints one path per line to
stdout, and nothing else, so it pipes cleanly.

```bash
videre dedupe                          # list removable copies (one path per line)
videre dedupe | xargs trash            # ...and delete them
videre dedupe --similar                # also report look-alike groups (review only)
videre dedupe --db ~/photos.db         # use a specific database
videre dedupe --silent                 # suppress the summary; paths still print
videre dedupe --json                   # print one JSON object instead
```

:::danger
This prints the REMOVE side of each duplicate group, so
`videre dedupe | xargs trash` deletes those files immediately. Run
[`videre report`](/commands/report/) first to review the KEEP/REMOVE badges, or
write the list to a file and read it.
:::

## Which copy is kept

Within each group, the KEEP candidate is the one with the oldest EXIF date. If
that is absent it falls back to the earlier of the created and modified
timestamps. EXIF dates of `0000-00-00T00:00:00`, which cameras with an unset
clock produce, are treated as absent.

## `--similar` is review-only

Look-alike groups are deliberately kept out of stdout, so piping into a delete
command can never act on a mere resemblance. They appear in the summary on
stderr and in [`videre report`](/commands/report/), where you can look at them.

This needs a prior [`videre scan --similar`](/commands/scan/) to have computed
the fingerprints.

## Output streams

| Stream | Contents |
|---|---|
| stdout | REMOVE candidate paths, one per line |
| stderr | Progress and summary, suppressed by `--silent` |

With `--json`, stdout is instead a single JSON object — always, including an
error object plus a nonzero exit code on failure.
