# videre docs

The source for <https://docs.videre.sh>, built with
[Astro](https://astro.build) + [Starlight](https://starlight.astro.build).

```bash
yarn install      # once
yarn dev          # http://localhost:4321
yarn build        # static site into dist/
yarn preview      # serve dist/ locally
```

Yarn 4, pinned by the `packageManager` field in `package.json`. To have that
version enforced exactly rather than falling back to whatever `yarn` is on your
`PATH`:

```bash
corepack enable
```

:warning: This project sets `nodeLinker: node-modules` in `.yarnrc.yml`. Yarn's
default PnP linker **does not work here**: Astro resolves virtual module
specifiers such as `astro:toolbar:internal`, which are not real packages, and
PnP rejects them as unsound. The build fails at the bundling stage. Please don't
"modernise" this back to PnP without re-testing a full `yarn build`.

## Layout

```
src/content/docs/
  index.mdx          landing page
  start/             install, quickstart, cautions
  commands/          one page per subcommand
  reference/         paths, platforms, file types, models
astro.config.mjs     sidebar, site URL, theme
```

Adding a page means creating the file and adding it to the `sidebar` array in
`astro.config.mjs`. The right-hand table of contents, search indexing, and
light/dark theming all happen automatically.

Search is [Pagefind](https://pagefind.app), built at compile time into static
assets. There is no search service, no API key, and nothing leaves the reader's
browser.

## Deploying

`yarn build` produces plain static files in `dist/`, so any static host works.
The site is served from Cloudflare Pages at <https://docs.videre.sh>.

Project settings:

| Setting | Value |
|---|---|
| Root directory | `docs` |
| Build command | `corepack enable && yarn install --immutable && yarn build` |
| Build output directory | `dist` |
| Node version | from `.node-version` (24.11.1) |

Two of those are load-bearing rather than boilerplate:

**`corepack enable` in the build command.** Cloudflare's build image ships Yarn
1 by default. Running it against this Yarn 4 lockfile fails, so corepack has to
be turned on first to honour the `packageManager` field in `package.json`.

**`.node-version` pins 24.11.1.** Astro 7 requires Node >= 22.12.0, verified the
hard way: a build on 22.11.0 refuses to start with `Node.js v22.11.0 is not
supported by Astro`. Pinning a bare major would leave that to chance.

`--immutable` makes the build fail if `yarn.lock` is out of date, rather than
silently resolving different versions than the ones tested locally.

## Where things belong

- **README.md** (repo root) is the GitHub and crates.io front page: what videre
  is, how to install it, a short quickstart, and links here.
- **This site** is the user-facing reference: every command, every flag, and how
  the pieces fit together.
- **CLAUDE.md** is for people and agents working *on* videre: build and test
  invariants, measured findings, and the traps that are easy to reintroduce.

When something is true for users, it belongs here, and the other two link to it
rather than restating it.
