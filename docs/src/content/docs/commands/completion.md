---
title: videre completion
description: Print a shell completion script for bash, zsh, or fish.
---

Prints a shell completion script, generated from the same command definition
`videre` parses with, so the scripts never drift from the real flags. People
do not normally call this: [bash and fish install themselves](/start/install/#shell-completion)
on the first run, and the Homebrew formula generates the scripts at install
time. It is plumbing for installers and for hand wiring.

```bash
videre completion bash > ~/.local/share/bash-completion/completions/videre.bash
videre completion zsh > ~/.oh-my-zsh/completions/_videre   # then rehash
videre completion fish > ~/.config/fish/completions/videre.fish
```

The script is generated per binary, not per library: `--library` is accepted
here as on every command, but the output is identical either way.

zsh has no per-user completion directory on its load path, so its script is
saved wherever your `fpath` looks and `rehash` picks it up. See
[the install guide's completion section](/start/install/#shell-completion) for
the full story, including dynamic value completion for `--person`, `--model`,
`--ext` and `--mime`, and for the terms of a
[query](/reference/query-syntax/#completion).
