# Contributing to videre

videre is a terminal tool for managing a local media library, and it is free
software. It is written and maintained by one person, and its direction is set
by that person.

**Bug reports are genuinely useful and always welcome.** A clear reproduction
against a real library is worth more than most patches.

**If you hit a real problem in your own library, or want a feature you would
use yourself, you are welcome here.** Those are the contributions worth having,
and they are worth having because they come from someone who actually needs the
result.

What this project is not looking for is contribution for its own sake. There is
no good-first-issue list and no ambition to grow a contributor base.

For anything beyond a small fix, open an issue and agree the approach before
writing code. Direction is deliberate and not every change fits, and a rejected
pull request wastes your time more than mine, which is a bad trade for both of
us. If you want something videre is not going to do, the licence lets you fork
it, and that is a legitimate outcome rather than a failure.

## Building and testing

```bash
cargo build --release
make fmt-check                 # what CI enforces
cargo test --workspace
```

CI runs `make fmt-check` plus the full suite on Ubuntu and macOS. Both must be
green.

Worth knowing before changing anything:

- **Tests never download model weights.** Anything needing them either skips on
  a cold cache or is structured so the model is never loaded. A test that pulls
  weights slows every run for everyone.
- **Behaviour changes update the docs in the same commit.** User-facing
  documentation lives in `docs/src/content/docs/` and is published at
  <https://docs.videre.sh>.
- **Anything two subcommands need belongs in `videre-core`.** Look there for an
  existing helper before adding one to a command module.
- `CLAUDE.md` records the constraints and the measured findings behind the
  defaults. Most surprising code has a reason written down there.

macOS is the primary platform. Linux works with one gap: HEIC photos and video
frames are decoded through a macOS system tool, so those files are skipped for
thumbnails, search and face detection on Linux.

## Licensing of contributions

videre is licensed under the Apache License 2.0. If a contribution of yours is
merged, you confirm that:

- the contribution is your own work, or you otherwise have the right to submit
  it,
- it is contributed under the Apache License 2.0, like the rest of the project,
  and
- you grant the project maintainer a perpetual, worldwide, non-exclusive,
  transferable and sublicensable right to use your contribution, **including the
  right to distribute it under different licence terms in future versions of
  videre**.

**You keep the copyright in what you write.** This exists so the project can
change its licence later without having to find and ask every past contributor,
a problem that has stalled other projects for years. It takes nothing away from
you: your contribution remains available under the Apache License 2.0 in every
release it has appeared in, permanently.
