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

Any static host works, since `npm run build` produces plain files in `dist/`.
For Cloudflare Pages: root directory `docs`, build command `npm run build`,
output directory `dist`.

## Where things belong

- **README.md** (repo root) is the GitHub and crates.io front page: what videre
  is, how to install it, a short quickstart, and links here.
- **This site** is the user-facing reference: every command, every flag, and how
  the pieces fit together.
- **CLAUDE.md** is for people and agents working *on* videre: build and test
  invariants, measured findings, and the traps that are easy to reintroduce.

When something is true for users, it belongs here, and the other two link to it
rather than restating it.
