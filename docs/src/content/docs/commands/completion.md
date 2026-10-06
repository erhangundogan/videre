---
title: videre completion
description: Print a shell completion script for bash, zsh, fish, elvish or powershell.
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

`elvish` and `powershell` are accepted too, and print clap's script for that
shell.

The fish script also completes `videre config set`: its keys, then the
choices for the key you typed (the three provided models for `model`, `db`,
`file` or `newest` for `xmp`, and so on), with no dynamic completer needed.

Subcommands and fixed values complete in every shell: `videre dedupe` offers
its actions (`list`, `review`, `trash`, `delete`, `undo`), and `--kind` its
kinds (`exact`, `resized`, `creation`, `similar`).

The script is generated per binary, not per library: `--library` is accepted
here as on every command, but the output is identical either way.

zsh has no per-user completion directory on its load path, so its script is
saved wherever your `fpath` looks and `rehash` picks it up. See
[the install guide's completion section](/start/install/#shell-completion) for
the full story, including dynamic value completion for `--person`, `--model`,
`--tag`, `--category`, `--label`, `--ext` and `--mime`, for `videre config
set`'s values, and for the terms of a [query](/reference/query-syntax/#completion).
