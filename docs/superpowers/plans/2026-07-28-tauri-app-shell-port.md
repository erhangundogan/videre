# Tauri App Shell Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the Tauri desktop app's (`app/`) current top-nav shell (`ApplicationShell4`) with a sidebar shell ported from `videre-web`'s `application-shell11`, per `docs/superpowers/specs/2026-07-28-tauri-app-shell-port-design.md`.

**Architecture:** Add the missing shadcn UI primitives to `app/`'s existing Tailwind v3/`"new-york"` setup via the shadcn CLI (not copied verbatim from `videre-web`, which is Tailwind v4). Hand-port `application-shell11.tsx`'s structure into `app/`, rewriting its few Tailwind v4-only class syntaxes into v3-compatible equivalents, wiring its nav to React Router instead of placeholder `href="#"` links, and simplifying the user dropdown per the approved design (no name/email, no Account/Billing, "Quit" instead of "Log out"). No backend/data changes - existing routes (`LabelingPage`, `ClusterPage`, `PersonPage`) are unaffected, just re-wrapped by the new shell.

**Tech Stack:** React 19, TypeScript, Vite, Tailwind v3, shadcn/ui (`"new-york"` style, individual `@radix-ui/react-*` packages), react-router-dom v7, Tauri v2.

**Note on testing:** `app/` has no automated component-test framework (matching `videre-web` and the rest of this project's frontend work) - "tests" here mean TypeScript build (`tsc --noEmit` via `npm run build`) plus a final manual/visual verification pass via `npm run tauri dev`, the same convention Plan 3 (the original faces-labeling UI) used.

---

### Task 1: Add missing shadcn UI primitives

**Files:**
- Create (via CLI): `app/src/components/ui/sidebar.tsx`, `app/src/components/ui/collapsible.tsx`, `app/src/components/ui/input-group.tsx`, `app/src/components/ui/kbd.tsx`, `app/src/components/ui/skeleton.tsx`, `app/src/components/ui/textarea.tsx`, `app/src/components/ui/tooltip.tsx`
- Create (via CLI, will be overwritten in Task 2): `app/src/hooks/use-mobile.ts`

`application-shell11` needs `Sidebar` (+ its subcomponents), `Collapsible`, `InputGroup`, `Kbd`, `Skeleton`, `Textarea`, and `Tooltip`, none of which exist yet in `app/src/components/ui/` (which currently only has avatar, badge, button, card, dialog, dropdown-menu, input, scroll-area, separator, sheet). These are free `@shadcn/*` registry components - no `SHADCNBLOCKS_API_KEY` needed.

- [ ] **Step 1: Run the shadcn CLI against `app/`'s own config**

```bash
cd app
npx shadcn@latest add sidebar collapsible input-group kbd skeleton textarea tooltip
```

This targets `app/components.json` (Tailwind v3, `"new-york"`, individual `@radix-ui/*` packages) - it will add whatever `@radix-ui/react-*` packages each new component needs to `app/package.json` automatically, and will also generate `app/src/hooks/use-mobile.ts` (the sidebar's mobile-detection hook) - that generated version has a known lint issue that Task 2 replaces, so don't worry about its contents yet.

- [ ] **Step 2: Verify the build still passes**

```bash
npm run build
```

Expected: TypeScript compiles and Vite builds successfully, matching the same "Finished `tsc`, built in Xs" style output you'd see from a clean build today - no new errors. Warnings about newly added unused exports are fine at this stage since nothing imports the new components yet.

- [ ] **Step 3: Commit**

```bash
git add app/package.json app/package-lock.json app/src/components/ui/sidebar.tsx app/src/components/ui/collapsible.tsx app/src/components/ui/input-group.tsx app/src/components/ui/kbd.tsx app/src/components/ui/skeleton.tsx app/src/components/ui/textarea.tsx app/src/components/ui/tooltip.tsx app/src/hooks/use-mobile.ts
git commit -m "feat(app): add shadcn Sidebar/Collapsible/InputGroup/Kbd/Skeleton/Textarea/Tooltip primitives"
```

(If the CLI touched other lockfiles or added other generated files, include them too - check `git status` in `app/` first.)

---

### Task 2: Fix the generated `use-mobile` hook

**Files:**
- Modify: `app/src/hooks/use-mobile.ts`

The shadcn CLI ships a version of this hook that calls `setState` synchronously inside a `useEffect` (to seed the initial value), which trips `eslint-plugin-react-hooks`'s `set-state-in-effect` rule and causes an extra render right after mount - this was already found and fixed the same way in `videre-web` (see its git history). Port that fix here too rather than re-introducing the same issue.

- [ ] **Step 1: Replace the hook's contents**

Replace the entire contents of `app/src/hooks/use-mobile.ts` with:

```typescript
import * as React from "react"

const MOBILE_BREAKPOINT = 768

function subscribe(callback: () => void) {
  const mql = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT - 1}px)`)
  mql.addEventListener("change", callback)
  return () => mql.removeEventListener("change", callback)
}

function getSnapshot() {
  return window.innerWidth < MOBILE_BREAKPOINT
}

function getServerSnapshot() {
  return false
}

export function useIsMobile() {
  return React.useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot)
}
```

- [ ] **Step 2: Verify the build still passes**

```bash
npm run build
```

Expected: same clean build as Task 1's Step 2 - this hook isn't imported by anything yet (that happens in Task 5), so there's nothing new to exercise, just confirm no syntax/type errors.

- [ ] **Step 3: Commit**

```bash
git add app/src/hooks/use-mobile.ts
git commit -m "fix(app): replace use-mobile's effect+setState with useSyncExternalStore"
```

---

### Task 3: Port the videre logo component

**Files:**
- Create: `app/src/components/videre-logo.tsx`

**Reference:** `videre-web`'s `src/components/videre-logo.tsx` (repo at `/Users/erhangundogan/projects/videre-web`) - a plain inline SVG component using `currentColor`, no Tailwind-version-specific styling, so it ports unchanged.

- [ ] **Step 1: Create the file with this exact content**

```tsx
export function VidereLogo({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 405 400" fill="none" xmlns="http://www.w3.org/2000/svg" className={className}>
      <g stroke="currentColor">
        <g fill="currentColor">
          <circle cx="16" cy="69" r="15.5" />
          <circle cx="205" cy="16" r="15.5" />
          <circle cx="389" cy="69" r="15.5" />
          <circle cx="205" cy="376" r="23.5" />
        </g>
        <path d="m36.2942 102.294 136.7198 236.806" strokeLinecap="round" strokeWidth="18" />
        <path d="m204.07 330.983v-273.4397" strokeLinecap="round" strokeWidth="18" />
        <path d="m233.706 339.1 136.72-236.806" strokeLinecap="round" strokeWidth="18" />
      </g>
    </svg>
  );
}
```

- [ ] **Step 2: Verify the build still passes**

```bash
npm run build
```

Expected: clean build - this component isn't imported anywhere yet either, just confirm it's valid TSX.

- [ ] **Step 3: Commit**

```bash
git add app/src/components/videre-logo.tsx
git commit -m "feat(app): add VidereLogo component (aperture-V mark)"
```

---

### Task 4: Add the Tauri process plugin (for Quit)

**Files:**
- Modify: `app/package.json`
- Modify: `app/src-tauri/Cargo.toml`
- Modify: `app/src-tauri/src/lib.rs`
- Modify: `app/src-tauri/capabilities/default.json`

Tauri v2 moved process control (exiting the app) out of the core API into a plugin. This mirrors how `tauri-plugin-opener` is already integrated - same pattern, new plugin.

- [ ] **Step 1: Add the npm package**

```bash
cd app
npm install @tauri-apps/plugin-process
```

- [ ] **Step 2: Add the Rust crate**

Edit `app/src-tauri/Cargo.toml`, in the `[dependencies]` section, add this line right after `tauri-plugin-opener = "2"`:

```toml
tauri-plugin-process = "2"
```

- [ ] **Step 3: Register the plugin**

Edit `app/src-tauri/src/lib.rs`. Change:

```rust
        .plugin(tauri_plugin_opener::init())
```

to:

```rust
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_process::init())
```

- [ ] **Step 4: Add the permission**

Edit `app/src-tauri/capabilities/default.json`. Change:

```json
  "permissions": [
    "core:default",
    "opener:default"
  ]
```

to:

```json
  "permissions": [
    "core:default",
    "opener:default",
    "process:allow-exit"
  ]
```

- [ ] **Step 5: Verify the Rust side builds**

```bash
cd app/src-tauri
cargo build
```

Expected: `Compiling app v0.2.0 (.../app/src-tauri)` then `Finished` with no errors. This pulls in the new `tauri-plugin-process` crate and its dependencies.

- [ ] **Step 6: Verify the frontend still builds**

```bash
cd app
npm run build
```

Expected: clean build - the new npm package isn't imported yet (that's Task 5), just confirm nothing broke.

- [ ] **Step 7: Commit**

```bash
git add app/package.json app/package-lock.json app/src-tauri/Cargo.toml app/src-tauri/Cargo.lock app/src-tauri/src/lib.rs app/src-tauri/capabilities/default.json
git commit -m "feat(app): add tauri-plugin-process for app Quit action"
```

---

### Task 5: Port `ApplicationShell11`

**Files:**
- Create: `app/src/components/application-shell11.tsx`

This is the main port. Differences from `videre-web`'s version (all per the approved design spec):

1. **Tailwind v3 syntax fixes** - `videre-web` is Tailwind v4 and uses v4-only shorthands that don't exist in v3:
   - `h-(--header-height)` -> `h-[var(--header-height)]` (same for every `*-(--foo)` arbitrary-var shorthand)
   - `w-(--sidebar-width-icon)` -> `w-[var(--sidebar-width-icon)]`
   - `top-(--header-height)` -> `top-[var(--header-height)]`
   - `pt-(--header-height)` -> `pt-[var(--header-height)]`
   - `h-[calc(100svh-var(--header-height))]!` (v4's trailing-`!important`) -> `!h-[calc(100svh-var(--header-height))]` (v3's leading-`!important`)
   - `[--header-height:calc(--spacing(14))]` (v4's `--spacing()` theme function) -> `[--header-height:3.5rem]` (the plain value - Tailwind's default spacing step is `0.25rem`, so step 14 = `3.5rem`)
2. **`ApplicationShell11` takes `children`** instead of hardcoding `<ContentGrid />` - it wraps real routed pages (`LabelingPage`/`ClusterPage`/`PersonPage` via `<Outlet />`), not a static demo stub.
3. **Nav items route via React Router**, not placeholder `href="#"` - `Home` and `Face Detection` both go to `/` (today's only real feature); `Scan`/`Dedupe`/`Fix Dates`/`Smart Photos` render disabled (no route, `aria-disabled`, dimmed, "Coming soon" tooltip).
4. **User dropdown simplified**: no name/email label, no Account/Billing/Notifications/"Upgrade to Pro" items (SaaS-account concepts with no equivalent in a local desktop app) - just a single "Quit" item that calls `exit(0)` from `@tauri-apps/plugin-process`. The dropdown *content* (`AccountMenuContent`) is shared between desktop and mobile - `videre-web`'s mobile dropdown had extra items the desktop one didn't, and this port makes both consistent since neither set of extra items applies here. The trigger *buttons* stay distinct per the original design: desktop shows avatar + a chevron (`DesktopAccountMenu`), mobile is an icon-only square button with just the avatar, no chevron (`MobileAccountMenu`) - unifying those into one trigger component would have added a chevron to the mobile trigger that the original never had.
5. **Kept as-is** (per explicit "rest is good" confirmation on the design): the search bar (visually present, not wired to anything - out of scope), the sidebar footer's Terms/Privacy links (harmless placeholder links), the overall two-layout (desktop sidebar / mobile bottom-nav) structure.

- [ ] **Step 1: Create the file**

```tsx
import {
  Album,
  BrushCleaning,
  Calendar,
  ChevronDown,
  Home,
  LogOut,
  type LucideIcon,
  Menu,
  Scan,
  ScanFace,
  Search,
} from "lucide-react";
import * as React from "react";
import { Link, useLocation } from "react-router-dom";
import { exit } from "@tauri-apps/plugin-process";

import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import { VidereLogo } from "@/components/videre-logo";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { InputGroup, InputGroupAddon, InputGroupInput } from "@/components/ui/input-group";
import { Kbd } from "@/components/ui/kbd";
import { ScrollArea } from "@/components/ui/scroll-area";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarInset,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  useSidebar,
} from "@/components/ui/sidebar";
import { cn } from "@/lib/utils";

function getInitials(name: string) {
  return (
    name
      .split(" ")
      .filter(Boolean)
      .slice(0, 2)
      .map((part) => part[0]?.toUpperCase())
      .join("") || "U"
  );
}

const user = {
  name: "Erhan Gundogan",
  avatar: "",
};

type NavItem = {
  title: string;
  icon: LucideIcon;
  to?: string;
};

const navPrimary: NavItem[] = [
  { title: "Home", icon: Home, to: "/" },
  { title: "Scan", icon: Scan },
  { title: "Dedupe", icon: BrushCleaning },
  { title: "Fix Dates", icon: Calendar },
  { title: "Face Detection", icon: ScanFace, to: "/" },
  { title: "Smart Photos", icon: Album },
];

function NavPrimary({ items }: { items: NavItem[] }) {
  const location = useLocation();
  return (
    <SidebarGroup>
      <SidebarMenu>
        {items.map((item) =>
          item.to ? (
            <SidebarMenuItem key={item.title}>
              <SidebarMenuButton asChild isActive={location.pathname === item.to} tooltip={item.title}>
                <Link to={item.to}>
                  <item.icon />
                  <span>{item.title}</span>
                </Link>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ) : (
            <SidebarMenuItem key={item.title}>
              <SidebarMenuButton disabled aria-disabled tooltip={`${item.title} (coming soon)`}>
                <item.icon />
                <span>{item.title}</span>
              </SidebarMenuButton>
            </SidebarMenuItem>
          ),
        )}
      </SidebarMenu>
    </SidebarGroup>
  );
}

function FooterLinks() {
  return (
    <div className="text-muted-foreground px-4 py-4 text-xs">
      <div className="mt-2 flex flex-wrap gap-x-2 gap-y-1">
        <a href="#" className="hover:underline">
          Terms
        </a>
        <a href="#" className="hover:underline">
          Privacy
        </a>
      </div>
    </div>
  );
}

function AccountMenuContent() {
  return (
    <DropdownMenuContent className="w-40" align="end">
      <DropdownMenuItem onClick={() => exit(0)}>
        <LogOut className="mr-2 size-4" />
        Quit
      </DropdownMenuItem>
    </DropdownMenuContent>
  );
}

function DesktopAccountMenu() {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" className="flex h-auto items-center gap-2 px-2 py-1">
          <Avatar className="size-8">
            <AvatarImage src={user.avatar} alt={user.name} />
            <AvatarFallback>{getInitials(user.name)}</AvatarFallback>
          </Avatar>
          <ChevronDown className="text-muted-foreground size-3" />
        </Button>
      </DropdownMenuTrigger>
      <AccountMenuContent />
    </DropdownMenu>
  );
}

function MobileAccountMenu() {
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" size="icon" className="size-9">
          <Avatar className="size-8">
            <AvatarImage src={user.avatar} alt={user.name} />
            <AvatarFallback>{getInitials(user.name)}</AvatarFallback>
          </Avatar>
        </Button>
      </DropdownMenuTrigger>
      <AccountMenuContent />
    </DropdownMenu>
  );
}

function SiteHeader() {
  const { toggleSidebar } = useSidebar();

  return (
    <header className="bg-background fixed top-0 z-50 hidden w-full items-center border-b md:flex">
      <div className="flex h-[var(--header-height)] w-[var(--sidebar-width-icon)] shrink-0 items-center justify-center">
        <Button className="size-9" variant="ghost" size="icon" onClick={toggleSidebar}>
          <Menu className="size-5" />
        </Button>
      </div>
      <div className="flex h-[var(--header-height)] items-center pr-4">
        <Link to="/" className="flex items-center gap-2">
          <div className="bg-primary flex size-8 items-center justify-center rounded-sm">
            <VidereLogo className="text-primary-foreground size-5" />
          </div>
          <span className="hidden text-lg font-semibold sm:block">Videre</span>
        </Link>
      </div>

      <div className="flex flex-1 justify-center px-4">
        <InputGroup className="h-10 max-w-xl rounded-full">
          <InputGroupAddon>
            <Search className="text-muted-foreground" />
          </InputGroupAddon>
          <InputGroupInput placeholder="Search" />
          <InputGroupAddon align="inline-end">
            <Kbd>⌘K</Kbd>
          </InputGroupAddon>
        </InputGroup>
      </div>

      <div className="flex h-[var(--header-height)] items-center gap-1 px-4">
        <DesktopAccountMenu />
      </div>
    </header>
  );
}

function AppSidebar({ ...props }: React.ComponentProps<typeof Sidebar>) {
  return (
    <Sidebar
      collapsible="icon"
      className="top-[var(--header-height)] hidden !h-[calc(100svh-var(--header-height))] md:flex"
      {...props}
    >
      <SidebarContent className="overflow-hidden">
        <ScrollArea className="min-h-0 flex-1">
          <NavPrimary items={navPrimary} />
        </ScrollArea>
      </SidebarContent>
      <SidebarFooter className="group-data-[collapsible=icon]:hidden">
        <FooterLinks />
      </SidebarFooter>
    </Sidebar>
  );
}

function MobileHeader() {
  return (
    <header className="bg-background sticky top-0 z-50 flex h-14 items-center justify-between border-b px-4 md:hidden">
      <Link to="/" className="flex items-center gap-2">
        <div className="bg-primary flex size-8 items-center justify-center rounded-sm">
          <VidereLogo className="text-primary-foreground size-5" />
        </div>
        <span className="text-lg font-semibold">Videre</span>
      </Link>
      <div className="flex items-center gap-1">
        <Button variant="ghost" size="icon" className="size-9">
          <Search className="size-5" />
          <span className="sr-only">Search</span>
        </Button>
        <MobileAccountMenu />
      </div>
    </header>
  );
}

function MobileBottomNav() {
  const location = useLocation();
  return (
    <nav className="bg-background/95 fixed inset-x-0 bottom-0 z-40 border-t backdrop-blur md:hidden">
      <div className="grid grid-cols-4">
        {navPrimary.map((item) => {
          const Icon = item.icon;
          const isActive = item.to != null && location.pathname === item.to;
          const className = cn(
            "flex flex-col items-center gap-1 py-2 text-xs transition-colors",
            isActive ? "text-foreground" : "text-muted-foreground hover:text-foreground",
            !item.to && "opacity-50",
          );
          return item.to ? (
            <Link key={item.title} to={item.to} className={className}>
              <Icon className="size-5" />
              <span aria-hidden="true">{item.title}</span>
            </Link>
          ) : (
            <button key={item.title} type="button" disabled aria-disabled className={className}>
              <Icon className="size-5" />
              <span aria-hidden="true">{item.title}</span>
            </button>
          );
        })}
      </div>
    </nav>
  );
}

export function ApplicationShell11({ children }: { children: React.ReactNode }) {
  return (
    <div className="w-full [--header-height:3.5rem]">
      <SidebarProvider
        className="flex flex-col"
        style={{ "--sidebar-width-icon": "3rem" } as React.CSSProperties}
      >
        {/* Desktop layout */}
        <SiteHeader />
        <div className="hidden flex-1 pt-[var(--header-height)] md:flex">
          <AppSidebar />
          <SidebarInset>{children}</SidebarInset>
        </div>

        {/* Mobile layout */}
        <div className="flex flex-col md:hidden">
          <MobileHeader />
          <div className="pb-16">{children}</div>
          <MobileBottomNav />
        </div>
      </SidebarProvider>
    </div>
  );
}
```

Note: this keeps `videre-web`'s original two-block structure (a `SiteHeader` that self-hides via its own `hidden md:flex` classes, plus a separate desktop content row and a separate mobile block, each independently toggled by breakpoint) - the only change from the source is `{children}` replacing the hardcoded `<ContentGrid />`/"Content Here" stub. `children` renders twice in the DOM (once per breakpoint-specific subtree) with only one visible at a time via CSS - this is the existing, correct pattern for this kind of responsive dual-layout shell, not something this port needs to change.

- [ ] **Step 2: Verify the build passes**

```bash
cd app
npm run build
```

Expected: clean build. This file isn't wired into `AppShell.tsx` yet (Task 6), so nothing renders it, but it must compile standalone with no type errors - in particular check that `@tauri-apps/plugin-process`'s `exit` import resolves (confirms Task 4's `npm install` worked) and that all the `@/components/ui/*` imports resolve (confirms Task 1 worked).

- [ ] **Step 3: Commit**

```bash
git add app/src/components/application-shell11.tsx
git commit -m "feat(app): port application-shell11 as the new app shell"
```

---

### Task 6: Wire the new shell into `AppShell` and remove the old one

**Files:**
- Modify: `app/src/components/AppShell.tsx`
- Delete: `app/src/components/application-shell4.tsx`

- [ ] **Step 1: Update `AppShell.tsx`**

Replace the entire contents of `app/src/components/AppShell.tsx` with:

```tsx
import { Outlet } from "react-router-dom";

import { ApplicationShell11 } from "@/components/application-shell11";

export function AppShell() {
  return (
    <ApplicationShell11>
      <Outlet />
    </ApplicationShell11>
  );
}
```

(This is a one-line swap: `ApplicationShell4` -> `ApplicationShell11`, matching the import from Task 5.)

- [ ] **Step 2: Delete the now-unused old shell**

```bash
git rm app/src/components/application-shell4.tsx
```

- [ ] **Step 3: Verify the build passes**

```bash
cd app
npm run build
```

Expected: clean build, no "unused import" or "module not found" errors from removing `application-shell4.tsx`.

- [ ] **Step 4: Commit**

```bash
git add app/src/components/AppShell.tsx
git commit -m "feat(app): wire ApplicationShell11 into AppShell, remove ApplicationShell4"
```

---

### Task 7: Manual verification

**Files:** none (verification only)

- [ ] **Step 1: Launch the app**

```bash
cd app
npm run tauri dev
```

Wait for the Tauri window to open. This requires the default database to already exist (`~/.videre/hashes.db` or your configured one) - if you see "no database found", that's expected/unrelated to this change; run `videre scan <dir>` first per the main project's README.

- [ ] **Step 2: Confirm the shell renders**

Check: the sidebar shows Home, Scan, Dedupe, Fix Dates, Face Detection, Smart Photos. Home and Face Detection are clickable and highlighted as active when on `/`. Scan, Dedupe, Fix Dates, and Smart Photos appear visually disabled (dimmed) and do not navigate when clicked. The Videre aperture-V logo appears in the header (both desktop sidebar-header and, if you resize the window narrow enough, the mobile header).

- [ ] **Step 3: Confirm existing functionality still works**

Click into a cluster or person from the faces-labeling page (exercises `/cluster/:id` and `/person/:name` - confirms the shell swap didn't break existing routes). Confirm face thumbnails still load (exercises the `videre-face://` protocol, unrelated to this change but a good regression check since it's the same window).

- [ ] **Step 4: Confirm Quit works**

Click the avatar in the top-right corner, then "Quit" in the dropdown. Expected: the Tauri window closes and the process exits (check with `ps aux | grep app` in another terminal if uncertain - the process should be gone).

- [ ] **Step 5: Report results**

No commit for this task - if any step fails, note exactly what broke (screenshot or error text) before deciding whether to fix forward or roll back the responsible task.
