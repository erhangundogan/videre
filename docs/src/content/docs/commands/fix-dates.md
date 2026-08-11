---
title: videre fix-dates
description: Set each file's date from the date the camera recorded.
---

Sets each file's modification time from its EXIF date.

:::danger[This is the only command that changes your files]
There is no undo. It asks for confirmation first, and `--dry-run` shows exactly
what it would do.
:::

```bash
videre fix-dates --dry-run             # show what would change, touch nothing
videre fix-dates                       # apply (asks for confirmation first)
videre fix-dates --yes                 # apply without asking (for scripts)
videre fix-dates --silent              # no per-file output
videre fix-dates --db ~/photos.db      # use a specific database
```

Only files that actually have an EXIF date are touched. Both KEEP and REMOVE
candidates are included, since REMOVE files are going to be deleted anyway.

## What it changes

Only the modification time. Creation time needs a macOS-only syscall and is not
supported.

EXIF dates are camera-local with no timezone, so they are interpreted as local
system time.

## Confirmation

Before changing anything it prints how many files will be touched and asks
`[y/N]`. Anything other than `y` or `yes` aborts with no changes, including
EOF — so a script with stdin closed aborts safely rather than proceeding.

`--yes` skips the prompt for scripted use. `--dry-run` never prompts, since it
changes nothing. The prompt is skipped entirely when there is nothing to do.

## Exit status

Exits nonzero if any file could not be updated. Files that no longer exist on
disk, such as duplicates you already deleted, are skipped and counted in the
summary rather than treated as errors.
