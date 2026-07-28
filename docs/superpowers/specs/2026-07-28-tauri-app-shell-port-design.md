# Tauri app shell port (from videre-web) - design

Date: 2026-07-28
Status: approved for planning
Scope: replace the Tauri desktop app's current top-nav shell with a port of
`videre-web`'s `application-shell11` sidebar shell (logo, nav, user area).
No new backend functionality - the faces-labeling workflow is unchanged.

## 1. Overview

The Tauri app (`app/`) currently wraps its routes in `ApplicationShell4`, a
simple top-nav bar with a single "Faces" destination
(`app/src/components/application-shell4.tsx`, `AppShell.tsx`). Separately,
the new `videre-web` repo (Next.js, a future web/SaaS frontend, see memory
`architecture-multiplatform-ui` and `project-roadmap`) has an
`application-shell11` component: a fuller sidebar shell with a logo, a
multi-item nav (Home, Scan, Dedupe, Fix Dates, Face Detection, Smart Photos),
a search bar, and a user avatar/dropdown in the header.

This spec ports that shell's design into the Tauri app as its new main page
chrome, replacing `ApplicationShell4`. This is purely a shell/chrome swap:
the faces-labeling workflow (`LabelingPage`, `ClusterPage`, `PersonPage`)
keeps working exactly as it does today, just wrapped by the new shell
instead of the old one.

## 2. Non-goals

- **No new backend functionality.** Scan/Dedupe/Fix Dates/Smart Photos have
  no real commands behind them yet (that's the separate "make the app
  operational" idea, parked earlier this session - not part of this work).
- **No Tailwind v4 upgrade.** `app/` stays on its current Tailwind v3 /
  `"new-york"` / individual `@radix-ui/react-*` package setup. Achieving
  pixel-parity with `videre-web`'s Tailwind v4 / `radix-nova` rendering was
  explicitly rejected in favor of lower risk to the existing, working faces
  UI (see Approach below).
- **No changes to `ClusterPage`/`PersonPage`/`LabelingPage` internals.**
  They keep rendering the same; only their wrapping shell changes.

## 3. Approach: minimal-risk port, not a Tailwind v4 upgrade

Two approaches were considered:

- **A (chosen): port onto `app/`'s existing setup.** Add only the shadcn
  primitives the shell needs that `app/` doesn't already have, via
  `npx shadcn add <name>` run against `app/`'s own `components.json`
  (Tailwind v3, `"new-york"`, individual `@radix-ui/*` packages) - not
  copied verbatim from `videre-web`. Then hand-port `application-shell11`'s
  JSX/structure onto that component set. Result: the same layout, nav, and
  logo as `videre-web`, but not pixel-identical (new-york's spacing/radius
  tokens differ slightly from `videre-web`'s radix-nova preset). Low risk -
  doesn't touch the Tailwind config or components the existing faces UI
  already depends on.
- **B (rejected): upgrade `app/` to Tailwind v4 + `radix-nova` first,**
  enabling a near-verbatim copy of `application-shell11.tsx`. Achieves true
  visual parity with `videre-web`, but is a Tailwind major-version upgrade
  touching every existing component in the Tauri app - real regression risk
  to the faces-labeling UI, for a cosmetic-consistency goal nobody asked
  for. Worth revisiting later only if/when full design-system parity across
  both frontends becomes an actual goal.

## 4. Component inventory

`app/src/components/ui/` currently has: avatar, badge, button, card, dialog,
dropdown-menu, input, scroll-area, separator, sheet.

`application-shell11` additionally needs: `Sidebar` (+ its `SidebarProvider`/
`SidebarContent`/`SidebarInset`/`SidebarMenu*` family), `Collapsible`,
`InputGroup`, `Kbd`, `Skeleton`, `Textarea`, `Tooltip`, and the
`useIsMobile` hook (`src/hooks/use-mobile.ts` - port the
`useSyncExternalStore`-based version already fixed in `videre-web`, not a
naive effect+setState one, to avoid re-introducing the
`react-hooks/set-state-in-effect` issue that was fixed there).

All added via `npx shadcn add sidebar collapsible input-group kbd skeleton
textarea tooltip` (free `@shadcn/*` components, no API key needed) against
`app/`'s existing config, then the `use-mobile` hook ported by hand since
it's not a registry component.

## 5. Nav structure

Same six items as `videre-web`: Home, Scan, Dedupe, Fix Dates, Face
Detection, Smart Photos.

- **Home** - the only enabled item. Routes to `/` (today's `LabelingPage` -
  unchanged).
- **Scan, Dedupe, Fix Dates, Smart Photos** - rendered disabled (no route,
  no click action). Exact disabled treatment (grayed out, tooltip, badge -
  whatever reads clearly as "not yet available") is an implementation
  detail for the plan, not a design-level decision.
- **Face Detection** - also routes to `/` (it's the same underlying
  workflow as "Home" today - there is only one real feature). Whether this
  duplicates Home's route or gets its own disabled treatment is left to the
  plan to resolve simply; either is acceptable since there's no functional
  difference to a user yet.

## 6. Logo

Port `videre-web`'s `src/components/videre-logo.tsx` (the aperture-V mark,
using `currentColor` so it inherits the header's text color) into
`app/src/components/videre-logo.tsx` unchanged - it's a plain inline SVG
component with no Tailwind-version-specific styling, so it ports directly.
Replaces whatever renders in the shell's header logo slot today.

## 7. User area

Simplified from `videre-web`'s version:

- **Remove** the user's name/email display in the dropdown label area (no
  auth/account concept in a local single-user desktop app).
- **Keep** the avatar/dropdown trigger itself (not removed entirely, per
  explicit confirmation).
- **Replace** "Log out" with **"Quit"**, which actually exits the Tauri
  app. This needs a new dependency: Tauri v2 moved process control into a
  plugin, not the core API - `@tauri-apps/plugin-process` (npm) +
  `tauri-plugin-process` (Rust crate, registered in `src-tauri/src/lib.rs`
  alongside the existing `tauri-plugin-opener`) + a `process:default` (or
  `process:allow-exit`) permission added to
  `src-tauri/capabilities/default.json`. The plan should treat adding this
  plugin as its own task, mirroring how `tauri-plugin-opener` was already
  integrated.
- **Remove** the Account/Billing dropdown items (`BadgeCheck`/`CreditCard`
  icons, SaaS-account concepts with no equivalent in this app).

## 8. Routing and existing pages

Unaffected. `AppShell.tsx` continues to be the layout route wrapping `/`,
`/cluster/:id`, `/person/:name` via React Router's `<Outlet />` - only the
component it renders (`ApplicationShell4` -> the new ported shell) changes.
`ClusterPage`/`PersonPage`/`LabelingPage` need no changes.

## 9. Verification

- `npm run tauri dev` (or equivalent) to visually confirm the new shell
  renders, Home routes to the existing faces-labeling UI unchanged, disabled
  nav items look and behave as disabled, and Quit actually exits the app.
- Existing behavior (drag-assign, cluster/person pages, image loading via
  `videre-face://`/`videre-original://` protocols) must keep working
  unchanged - this is a chrome swap, not a rewrite of those pages.
