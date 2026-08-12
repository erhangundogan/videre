---
title: Leaving Google Photos
description: Export your library with Takeout and turn the result into a folder you own.
---

Google Photos can be left, but what it hands you is a mess. This is the whole
path from export to a library you can actually search, written for someone who
has not used a terminal tool for this before.

There is no shortcut through an API: since March 2025 Google no longer lets
other applications read your library, so Takeout is the only route out. That is
fine. Takeout gives you the actual files, and files are what videre works on.

## 1. Ask Google for your photos

Go to [takeout.google.com](https://takeout.google.com), deselect everything, then
select **Google Photos** only.

Choose `.zip`, and set the file size to the largest offered. Fewer, bigger
archives are much less tedious than dozens of small ones.

Google emails you when it is ready. For a large library this takes hours, and
sometimes more than a day.

## 2. Extract it

Download every archive and extract them all into **one folder**. If Google split
your library across several files, they are meant to be merged back together.

```bash
mkdir ~/Takeout
cd ~/Downloads
for z in takeout-*.zip; do unzip -q "$z" -d ~/Takeout; done
```

You should end up with `~/Takeout/Google Photos/` containing folders like
`Photos from 2019` and any albums you made.

## 3. What Takeout got wrong

Look at the extracted folder and you will see two problems.

**Every photo has today's date.** Your capture dates are not in the files. They
are in the `.json` files sitting beside them, and nothing else reads those.

**Photos appear several times.** A photo in three albums is exported three
times, so a 40 GB library can extract to considerably more.

videre fixes both.

## 4. Restore the real dates

```bash
videre import ~/Takeout --dry-run
```

This changes nothing. It reports what it found:

```
Google Takeout at ~/Takeout
  12,431 media file(s) in 84 folder(s)
  11,902 matched a sidecar (95.7%)
     529 unmatched, left untouched
   8,113 would have their date corrected
```

The percentage is the number to watch. Above about 95% is normal. Far below
that suggests something unusual about the export, and is worth asking about
before continuing.

When it looks right, run it for real:

```bash
videre import ~/Takeout
```

It asks for confirmation before changing anything, because this does modify your
files: it sets each file's date from its sidecar. Nothing else about the file
changes.

:::note[Why some files are never matched]
Screenshots, some videos and a few edited copies arrive with no sidecar at all.
Those keep whatever date they have. It is not an error, and the count is
reported so you can see how many.
:::

## 5. Build the library

```bash
videre scan ~/Takeout
```

This records what you have in a database at `~/.videre/hashes.db`. It reads
every file, so on a large library it takes a while. Your photos are not
modified.

## 6. Remove the album duplicates

```bash
videre report                  # look at what would go
videre dedupe | xargs trash    # delete it
videre prune                   # tidy the database afterwards
```

`videre report` opens a page in your browser showing every duplicate group, with
KEEP and REMOVE badges. Look before you delete.

This is where the album duplication disappears. Those copies are byte-identical,
so removing them loses nothing at all.

:::caution
`videre dedupe | xargs trash` deletes immediately. Run `videre report` first, or
send the list to a file and read it. See [cautions](/start/cautions/).
:::

## 7. Make it searchable

Optional, and the reason to bother with any of this:

```bash
videre embed                   # one-time, downloads about 780 MB
videre search "sunset over water"

videre faces                   # one-time, downloads about 180 MB
videre report --faces          # name the people
videre search --person "Alice"
```

Both steps take hours on a large library and can be interrupted with Ctrl-C at
any point; rerunning continues where it stopped.

## Where you end up

A folder you own, on a disk you control, with no account and nothing syncing.
The photos are ordinary files: if you stop using videre tomorrow, they are
exactly as they are now.

```bash
videre stats
```

## Afterwards

Keep the Takeout archives until you have checked the result. Once you are
satisfied, they are just a duplicate copy of what you already have.

If you add photos later, the same three commands bring them in:

```bash
videre scan ~/Takeout --retry-incomplete
videre embed
videre faces
```

See [workflows](/start/workflows/) for what needs what.
