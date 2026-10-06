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

**Photos without a camera date have today's date.** WhatsApp and Viber images,
screenshots and scans carry no date of their own, so they arrive dated the day
you unpacked the export. Their real dates, and often their places, are in the
`.json` files sitting beside them, which most tools ignore.

**Photos appear several times.** A photo in three albums is exported three
times, so a 40 GB library can extract to considerably more.

**Edited photos appear twice.** Every photo you edited in Google Photos comes
as the original, `IMG_1.jpg`, and Google's render of the edit,
`IMG_1-edited.jpg`, side by side.

videre fixes both.

## 4. Check the export

```bash
videre import ~/Takeout
```

This changes nothing. It reports what it found:

```
Google Takeout at ~/Takeout
  12,431 media file(s) in 84 folder(s)
  11,902 matched a sidecar (95.7%)
     529 unmatched, left untouched
  11,902 with a capture date and 9,214 with a place; videre scan reads both
```

The percentage is the number to watch. Above about 95% is normal. Far below
that suggests something unusual about the export, and is worth asking about
before continuing.

:::note[Why some files are never matched]
Screenshots, some videos and a few edited copies arrive with no sidecar at all.
Those keep whatever date they have. It is not an error, and the count is
reported so you can see how many.
:::

## 5. Build the library

```bash
videre --library ~/Takeout scan
```

This records what you have in a database at `.videre/hashes.db` inside the
library. It reads every file, so on a large library it takes a while. A photo
with no camera date takes its date, and a photo with no GPS its place, from its
sidecar, so it lands on the right day and the right spot on the map.
[`videre status`](/commands/status/) shows how many dates came from sidecars.
Your photos are not modified.

Then, to give the files themselves those dates, so any other tool sorts them
correctly too:

```bash
videre --library ~/Takeout fix-dates
```

This one does change your files (only their modification times), so it asks
first. See [`fix-dates`](/commands/fix-dates/).

## 6. Remove the album duplicates

```bash
videre dedupe review                  # look at what would go
videre dedupe trash                 # move the copies to the trash, once you agree
videre prune                          # tidy the database afterwards
```

`videre dedupe review` opens a page in your browser showing every duplicate group, with
KEEP and REMOVE badges. Look before you delete.

This is where the album duplication disappears. Those copies are byte-identical,
so removing them loses nothing at all.

:::caution
`videre dedupe trash` deletes immediately once you confirm. Run `videre
dedupe review` first to review, or preview with `trash --dry-run`.
Changed your mind? `videre dedupe undo` puts the last run back.
`delete` removes the copies permanently instead: faster, with no undo. Use
either rather than piping the list to another tool, which splits Takeout
paths on their spaces. See [cautions](/reference/cautions/).
:::

### Then the edited copies

The `-edited` renders are not byte-identical to their originals, so the step
above leaves them. `videre import` counts them and says so; remove them with:

```bash
videre dedupe review --kind creation         # each original beside its edit
videre dedupe trash --kind creation        # keep the originals, trash the creations
videre dedupe undo                  # changed your mind? put the edits back
```

The original is kept because it is the file the camera wrote; the edit is
Google's re-encoded copy. Faces you named on an edit are not moved to its
original. See [`dedupe --kind creation`](/commands/dedupe/#google-photos-creations).

## 7. Make it searchable

Optional, and the reason to bother with any of this:

```bash
videre embed                   # one-time, downloads about 1.5 GB
videre search "sunset over water"

videre faces                   # one-time, downloads about 180 MB
videre gallery          # name the people
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
videre --library ~/Takeout scan
videre embed
videre faces
```

See [workflows](/start/workflows/) for what needs what.
