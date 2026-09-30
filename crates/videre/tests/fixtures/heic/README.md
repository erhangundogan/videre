# HEIC rotation fixtures

`grid_rot90.heic` is a synthetic test pattern, never a photograph, laid out
the way an iPhone saves a portrait shot:

- the primary item is a 2048x1536 `grid` of twelve 512x512 `hvc1` tiles;
- a 320x240 `hvc1` thumbnail;
- one `irot` property of 270 degrees (counter-clockwise), shared through
  `ipma` by the grid and the thumbnail, so it displays turned 90 degrees
  clockwise;
- EXIF Orientation 6, matching the `irot`.

The stored pixels carry a solid red 256x256 marker in their top-left corner,
so a decoded result shows where the rotation put it.

Regenerate on macOS with ImageIO:

```bash
swift make_fixture.swift grid_rot90.heic
```
