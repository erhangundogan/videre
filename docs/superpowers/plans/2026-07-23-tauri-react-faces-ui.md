# Tauri React/shadcn Faces UI - Implementation Plan (Plan 3 of 3)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the smoke-test `App.tsx` with the real face-labeling desktop UI (People / Unassigned Clusters / Singletons, plus cluster-detail and person-detail views), built with React + shadcn/ui over a swappable `VidereClient` interface, at feature parity with today's server-rendered `FACES_HTML`/`CLUSTER_HTML`/`PERSON_HTML` pages.

**Architecture:** All data flows through a typed `VidereClient` interface (components never call Tauri `invoke` directly), with a `TauriClient` implementation now and an `HttpClient` left as a future drop-in for the web/on-prem dashboard. TanStack Query hooks wrap the client and own cache invalidation. Images load via the existing `videre-face://<id>` / `videre-original://<id>` protocols. React Router provides the three routes.

**Tech Stack:** Existing Tauri v2 app at `app/` (Vite + React + TypeScript). Adds: Tailwind CSS + shadcn/ui, `@tanstack/react-query`, `react-router-dom`. Backend is unchanged - Plan 2 already exposes the 11 commands (`faces_list`, `cluster_detail`, `person_detail`, `search_person`, `assign`, `new_person`, `remove_face`, `dissolve_cluster`, `delete_person`, `set_primary`, `rename_person`) and the two image protocols. This is Plan 3 of 3; see `docs/superpowers/specs/2026-07-23-desktop-app-design.md`.

**Parity reference:** The behaviors to reproduce live in `crates/videre/src/commands/report.rs` as `FACES_HTML` (labeling page: People/Clusters/Singletons, name-sorted people, top/right sidebar toggle persisted in localStorage, drag-assign, singleton multi-select + bulk assign, New Person), `CLUSTER_HTML` (cluster detail: faces grid, per-face Remove/Assign, "Assign cluster", "Dissolve cluster"), and `PERSON_HTML` (person detail: faces grid, "Set Default" with a ★ Default badge, per-face Remove, rename, delete person). Read those for exact UX; this plan rebuilds them as React components.

---

## Command / type surface (already implemented in Plan 2)

Tauri commands and their arg/return shapes (camelCase is NOT used - Tauri serializes Rust snake_case fields as-is; args are passed as a JS object with snake_case keys):

| command | args | returns |
|---|---|---|
| `faces_list` | - | `FacesData { people: PersonData[], clusters: ClusterData[], singletons: SingletonData[] }` |
| `cluster_detail` | `{ cluster_id: number }` | `ClusterDetail { cluster_id, faces: ClusterFaceData[] }` |
| `person_detail` | `{ name: string }` | `PersonDetail { label, faces: PersonFaceData[] }` |
| `search_person` | `{ name: string }` | `string[]` (image paths) |
| `assign` | `{ face_ids: number[], person_label: string }` | `void` |
| `new_person` | `{ face_ids: number[], label: string }` | `void` |
| `remove_face` | `{ face_id: number }` | `void` |
| `dissolve_cluster` | `{ cluster_id: number }` | `void` |
| `delete_person` | `{ label: string }` | `void` |
| `set_primary` | `{ face_id: number, person_label: string }` | `void` |
| `rename_person` | `{ old_label: string, new_label: string }` | `void` |

Serde shapes (from `crates/videre-api/src/types.rs`):
- `PersonData { label: string; face_ids: number[]; representative_id: number; hashes: string[] }`
- `ClusterData { cluster_id: number; face_ids: number[]; hashes: string[] }`
- `SingletonData { face_id: number; hash: string }`
- `ClusterFaceData { face_id: number; hash: string; path: string }`
- `PersonFaceData { face_id: number; hash: string; path: string; is_primary: boolean }`

Errors: commands reject with a string (the `videre_api::Error` `Display`). `rename_person` onto an existing name rejects with `"conflict"`.

Image URLs: `videre-face://<id>` (140px thumbnail), `videre-original://<id>` (full image, open in new context). Both usable directly as `<img src>`.

---

## File Structure

- Modify: `app/package.json` - add deps (Tailwind, shadcn deps, react-query, react-router-dom)
- Create: `app/tailwind.config.js`, `app/postcss.config.js`, `app/src/index.css` (Tailwind directives + shadcn CSS vars)
- Modify: `app/src/main.tsx` - wrap in `QueryClientProvider`, `BrowserRouter`, `ClientProvider`; import `index.css`
- Create: `app/components.json` - shadcn config; `app/src/lib/utils.ts` - `cn()` helper
- Create: `app/src/lib/models.ts` - TS types mirroring the serde shapes
- Create: `app/src/lib/client.ts` - `VidereClient` interface + `TauriClient` + image URL helpers
- Create: `app/src/lib/ClientProvider.tsx` - React context + `useClient()`
- Create: `app/src/lib/queries.ts` - TanStack Query hooks (queries + mutations)
- Create: `app/src/components/ui/*` - shadcn primitives (button, card, input, badge, dialog) via shadcn CLI
- Create: `app/src/components/FaceImage.tsx` - `<img>` wrapper for `videre-face://`
- Create: `app/src/routes/LabelingPage.tsx` - the People/Clusters/Singletons page
- Create: `app/src/routes/ClusterPage.tsx` - `/cluster/:id`
- Create: `app/src/routes/PersonPage.tsx` - `/person/:name`
- Modify: `app/src/App.tsx` - route table only

---

## Task 1: Frontend tooling (Tailwind + shadcn + React Query + Router)

**Files:** `app/package.json`, `app/tailwind.config.js`, `app/postcss.config.js`, `app/src/index.css`, `app/components.json`, `app/src/lib/utils.ts`, `app/src/main.tsx`

- [ ] **Step 1: Install dependencies**

Run in `app/`:
```bash
npm install @tanstack/react-query react-router-dom
npm install -D tailwindcss@3 postcss autoprefixer
npx tailwindcss init -p
```
(Pin Tailwind v3 - shadcn/ui targets v3; v4 changes the config format.)

- [ ] **Step 2: Configure Tailwind content globs + shadcn CSS variables**

Set `app/tailwind.config.js` `content` to `["./index.html", "./src/**/*.{ts,tsx}"]`, add `darkMode: ["class"]`, and the shadcn theme extension (colors mapped to CSS vars). Replace `app/src/index.css` with the standard shadcn base: `@tailwind base; @tailwind components; @tailwind utilities;` plus the `:root` / `.dark` CSS-variable blocks (copy the default shadcn "slate" variables - the exact block is documented at ui.shadcn.com/docs/installation/vite; reproduce it verbatim here so no external fetch is needed at build time).

- [ ] **Step 3: shadcn init + utils**

Create `app/components.json` (shadcn config: style "default", tailwind config path, `@/` alias -> `src/`, RSC false, tsx true). Add the `@/*` path alias to `app/tsconfig.json` (`"paths": { "@/*": ["./src/*"] }`) and to `app/vite.config.ts` (`resolve.alias` mapping `@` to `path.resolve(__dirname, "./src")`). Create `app/src/lib/utils.ts`:
```ts
import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";
export function cn(...inputs: ClassValue[]) { return twMerge(clsx(inputs)); }
```
Run `npm install clsx tailwind-merge`.

- [ ] **Step 4: Add shadcn primitives**

Run: `npx shadcn@latest add button card input badge dialog` (creates `src/components/ui/*`). If the CLI prompts, accept defaults. Verify the files compile.

- [ ] **Step 5: Providers in main.tsx**

Rewrite `app/src/main.tsx`:
```tsx
import React from "react";
import ReactDOM from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BrowserRouter } from "react-router-dom";
import App from "./App";
import { ClientProvider } from "./lib/ClientProvider";
import { TauriClient } from "./lib/client";
import "./index.css";

const qc = new QueryClient({ defaultOptions: { queries: { retry: false, staleTime: 5_000 } } });

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <QueryClientProvider client={qc}>
      <ClientProvider client={new TauriClient()}>
        <BrowserRouter>
          <App />
        </BrowserRouter>
      </ClientProvider>
    </QueryClientProvider>
  </React.StrictMode>
);
```
(`ClientProvider`/`TauriClient` land in Task 2 - this file will not compile until then; that's expected, build at the end of Task 2.)

- [ ] **Step 6: Commit**
```bash
git add app && git commit -m "chore(app): Tailwind + shadcn + react-query + router tooling"
```

---

## Task 2: Models, VidereClient, and query hooks

**Files:** `app/src/lib/models.ts`, `app/src/lib/client.ts`, `app/src/lib/ClientProvider.tsx`, `app/src/lib/queries.ts`, `app/src/components/FaceImage.tsx`

- [ ] **Step 1: Types**

Create `app/src/lib/models.ts` mirroring the serde shapes exactly (snake_case keys):
```ts
export interface PersonData { label: string; face_ids: number[]; representative_id: number; hashes: string[]; }
export interface ClusterData { cluster_id: number; face_ids: number[]; hashes: string[]; }
export interface SingletonData { face_id: number; hash: string; }
export interface FacesData { people: PersonData[]; clusters: ClusterData[]; singletons: SingletonData[]; }
export interface ClusterFaceData { face_id: number; hash: string; path: string; }
export interface ClusterDetail { cluster_id: number; faces: ClusterFaceData[]; }
export interface PersonFaceData { face_id: number; hash: string; path: string; is_primary: boolean; }
export interface PersonDetail { label: string; faces: PersonFaceData[]; }
```

- [ ] **Step 2: VidereClient interface + TauriClient**

Create `app/src/lib/client.ts`:
```ts
import { invoke } from "@tauri-apps/api/core";
import type { FacesData, ClusterDetail, PersonDetail } from "./models";

export interface VidereClient {
  facesList(): Promise<FacesData>;
  clusterDetail(clusterId: number): Promise<ClusterDetail>;
  personDetail(name: string): Promise<PersonDetail>;
  searchPerson(name: string): Promise<string[]>;
  assign(faceIds: number[], personLabel: string): Promise<void>;
  newPerson(faceIds: number[], label: string): Promise<void>;
  removeFace(faceId: number): Promise<void>;
  dissolveCluster(clusterId: number): Promise<void>;
  deletePerson(label: string): Promise<void>;
  setPrimary(faceId: number, personLabel: string): Promise<void>;
  renamePerson(oldLabel: string, newLabel: string): Promise<void>;
  faceImageUrl(faceId: number): string;
  originalImageUrl(faceId: number): string;
}

export class TauriClient implements VidereClient {
  facesList() { return invoke<FacesData>("faces_list"); }
  clusterDetail(clusterId: number) { return invoke<ClusterDetail>("cluster_detail", { cluster_id: clusterId }); }
  personDetail(name: string) { return invoke<PersonDetail>("person_detail", { name }); }
  searchPerson(name: string) { return invoke<string[]>("search_person", { name }); }
  assign(faceIds: number[], personLabel: string) { return invoke<void>("assign", { face_ids: faceIds, person_label: personLabel }); }
  newPerson(faceIds: number[], label: string) { return invoke<void>("new_person", { face_ids: faceIds, label }); }
  removeFace(faceId: number) { return invoke<void>("remove_face", { face_id: faceId }); }
  dissolveCluster(clusterId: number) { return invoke<void>("dissolve_cluster", { cluster_id: clusterId }); }
  deletePerson(label: string) { return invoke<void>("delete_person", { label }); }
  setPrimary(faceId: number, personLabel: string) { return invoke<void>("set_primary", { face_id: faceId, person_label: personLabel }); }
  renamePerson(oldLabel: string, newLabel: string) { return invoke<void>("rename_person", { old_label: oldLabel, new_label: newLabel }); }
  faceImageUrl(faceId: number) { return `videre-face://${faceId}`; }
  originalImageUrl(faceId: number) { return `videre-original://${faceId}`; }
}
```
CRITICAL: the `invoke` arg keys MUST be snake_case (`cluster_id`, `face_ids`, `person_label`, `old_label`, `new_label`) to match the Tauri command signatures from Plan 2. A camelCase key silently passes `undefined`.

- [ ] **Step 3: Client context**

Create `app/src/lib/ClientProvider.tsx`:
```tsx
import { createContext, useContext, type ReactNode } from "react";
import type { VidereClient } from "./client";
const Ctx = createContext<VidereClient | null>(null);
export function ClientProvider({ client, children }: { client: VidereClient; children: ReactNode }) {
  return <Ctx.Provider value={client}>{children}</Ctx.Provider>;
}
export function useClient(): VidereClient {
  const c = useContext(Ctx);
  if (!c) throw new Error("useClient must be used within ClientProvider");
  return c;
}
```

- [ ] **Step 4: Query hooks**

Create `app/src/lib/queries.ts` with typed hooks. Queries: `useFacesList`, `useClusterDetail(id)`, `usePersonDetail(name)`. Mutations invalidate `["faces"]` and the relevant detail key on success:
```tsx
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { useClient } from "./ClientProvider";

export function useFacesList() {
  const c = useClient();
  return useQuery({ queryKey: ["faces"], queryFn: () => c.facesList() });
}
export function useClusterDetail(id: number) {
  const c = useClient();
  return useQuery({ queryKey: ["cluster", id], queryFn: () => c.clusterDetail(id) });
}
export function usePersonDetail(name: string) {
  const c = useClient();
  return useQuery({ queryKey: ["person", name], queryFn: () => c.personDetail(name) });
}
export function useMutations() {
  const c = useClient();
  const qc = useQueryClient();
  const inval = () => qc.invalidateQueries();
  return {
    assign: useMutation({ mutationFn: (v: { faceIds: number[]; label: string }) => c.assign(v.faceIds, v.label), onSuccess: inval }),
    newPerson: useMutation({ mutationFn: (v: { faceIds: number[]; label: string }) => c.newPerson(v.faceIds, v.label), onSuccess: inval }),
    removeFace: useMutation({ mutationFn: (id: number) => c.removeFace(id), onSuccess: inval }),
    dissolveCluster: useMutation({ mutationFn: (id: number) => c.dissolveCluster(id), onSuccess: inval }),
    deletePerson: useMutation({ mutationFn: (label: string) => c.deletePerson(label), onSuccess: inval }),
    setPrimary: useMutation({ mutationFn: (v: { faceId: number; label: string }) => c.setPrimary(v.faceId, v.label), onSuccess: inval }),
    renamePerson: useMutation({ mutationFn: (v: { oldLabel: string; newLabel: string }) => c.renamePerson(v.oldLabel, v.newLabel), onSuccess: inval }),
  };
}
```

- [ ] **Step 5: FaceImage component**
Create `app/src/components/FaceImage.tsx`:
```tsx
import { useClient } from "@/lib/ClientProvider";
export function FaceImage({ faceId, size = 140, className }: { faceId: number; size?: number; className?: string }) {
  const c = useClient();
  return (
    <img src={c.faceImageUrl(faceId)} width={size} height={size} loading="lazy" alt={`face ${faceId}`}
      className={className}
      style={{ objectFit: "cover", aspectRatio: "1 / 1", maxWidth: "100%", height: "auto", background: "#e5e7eb", borderRadius: 6 }}
      onError={(e) => { (e.currentTarget as HTMLImageElement).style.visibility = "hidden"; }} />
  );
}
```

- [ ] **Step 6: Build + commit**
Run: `cd app && npm run build` (should type-check now that client/provider exist). Fix any snake_case/type mismatches.
```bash
git add app && git commit -m "feat(app): VidereClient interface, TauriClient, query hooks, FaceImage"
```

---

## Task 3: Labeling page (People / Clusters / Singletons)

**Files:** `app/src/routes/LabelingPage.tsx`

Rebuild `FACES_HTML` behavior. Sections: **People** (blue), **Unassigned Clusters** (green), **Singletons** (orange). Requirements (parity with the current server UI, all already shipped there):

- People sorted by name (`label.localeCompare(other, undefined, { sensitivity: "base" })`), stable during drag.
- People placement toggle: sticky top bar vs fixed right sidebar; persist choice in `localStorage["videre_people_layout"]`, applied on load.
- Each person card: representative face thumbnail (`FaceImage faceId={representative_id}`), label linking to `/person/<name>`, "+N more" count. Drop target: dragging a cluster/singleton onto it calls `assign(faceIds, label)`.
- Each cluster card: up-to-4 face thumbnails + "+N more", links to `/cluster/<id>`, a drag handle (drag to a person), and a "New Person" inline input (calls `newPerson(faceIds, label)`).
- Singletons: same card shape (single face), PLUS click-the-thumbnail multi-select (checkmark overlay + highlight); a floating action bar when >=1 selected: "N selected · New Person · Clear"; dragging any selected singleton onto a person assigns the whole selection; "New Person" from the bar names all selected.
- Uses `useFacesList()` and `useMutations()`. After a mutation, the query invalidation refetches (no manual reload).

- [ ] **Step 1: Implement `LabelingPage.tsx`** as a component using `useFacesList()` (loading/error states via shadcn `Card`/skeleton), the three sections, HTML5 drag-and-drop (dragstart sets `application/json` `{ face_ids }`; person card `onDrop` parses and calls `assign`), and the singleton selection `Set<number>` state + floating bar. Name sanitization: trim + cap 60 chars client-side (mirror `sanitizeName`); the backend also sanitizes.
- [ ] **Step 2: Build** (`cd app && npm run build`) - fix types.
- [ ] **Step 3: Commit** `git add app && git commit -m "feat(app): labeling page (people/clusters/singletons, drag-assign, multi-select)"`

Acceptance: matches `FACES_HTML` behavior. Verify against `crates/videre/src/commands/report.rs` `FACES_HTML` for any detail (badge colors, sidebar CSS, multi-select bar copy).

---

## Task 4: Cluster detail page (`/cluster/:id`)

**Files:** `app/src/routes/ClusterPage.tsx`

Rebuild `CLUSTER_HTML`. Read `:id` from the route, `useClusterDetail(id)`. Show: header "Cluster {id} - {n} face(s)"; a faces grid (each `FaceImage` at ~180px, linking the thumbnail to `originalImageUrl` opened via `window.open` in a new webview/tab is not available - instead show it inline or in a shadcn `Dialog` lightbox); per-face **Remove** (`removeFace`) and **Assign** (inline person-name input -> `newPerson([faceId], label)`); a top bar with "Assign all to [input]" (`newPerson(allFaceIds, label)`) and **Dissolve cluster** (`dissolveCluster(id)`, then navigate back to `/`). A person-name `<datalist>` populated from the current people (from `useFacesList`) for autocomplete.

- [ ] **Step 1:** Implement `ClusterPage.tsx`. On dissolve/assign-all success, `navigate("/")`.
- [ ] **Step 2:** Build.
- [ ] **Step 3:** Commit `git add app && git commit -m "feat(app): cluster detail page (assign/remove/dissolve)"`

---

## Task 5: Person detail page (`/person/:name`)

**Files:** `app/src/routes/PersonPage.tsx`

Rebuild `PERSON_HTML`. Read `:name` (decode URI), `usePersonDetail(name)`. Show: header with the name, face count, a **rename** input (`renamePerson(name, newName)`; on `"conflict"` rejection show "A person named X already exists", on success `navigate("/person/"+encodeURIComponent(newName))`), and a **Remove person** button (`deletePerson(name)` with a confirm `Dialog`, then `navigate("/")`). Faces grid: each face card shows `FaceImage`, a "★ Default" badge + highlighted border when `is_primary`, a **Set Default** button (disabled when already primary; calls `setPrimary(faceId, name)`), and **Remove** (`removeFace`). After set-default, invalidation refetches so the badge moves (and the labeling page's thumbnail updates).

- [ ] **Step 1:** Implement `PersonPage.tsx`.
- [ ] **Step 2:** Build.
- [ ] **Step 3:** Commit `git add app && git commit -m "feat(app): person detail page (set-default/rename/delete/remove)"`

---

## Task 6: Routing, integration, verification

**Files:** `app/src/App.tsx`

- [ ] **Step 1: Route table** - replace `app/src/App.tsx`:
```tsx
import { Routes, Route } from "react-router-dom";
import { LabelingPage } from "./routes/LabelingPage";
import { ClusterPage } from "./routes/ClusterPage";
import { PersonPage } from "./routes/PersonPage";

export default function App() {
  return (
    <Routes>
      <Route path="/" element={<LabelingPage />} />
      <Route path="/cluster/:id" element={<ClusterPage />} />
      <Route path="/person/:name" element={<PersonPage />} />
    </Routes>
  );
}
```
Ensure each route component is a named export matching these imports.

- [ ] **Step 2: Frontend build** - `cd app && npm run build` (tsc + vite). Must pass clean.
- [ ] **Step 3: Backend build** - `cd app/src-tauri && cargo build`. Must pass.
- [ ] **Step 4: End-to-end run (human/manual)** - `cd app && npm run tauri dev`. Confirm: labeling page shows real People/Clusters/Singletons with thumbnails; drag a cluster onto a person assigns it; open a cluster, dissolve it; open a person, set a default (badge moves) and rename. If headless, SKIP the launch and report the build passing as the achievable verification (note it, like Plan 2 Task 6).
- [ ] **Step 5: Commit** `git add app && git commit -m "feat(app): wire routes for the faces UI"`

---

## Self-Review

- **Spec coverage:** the three views reproduce `FACES_HTML`/`CLUSTER_HTML`/`PERSON_HTML` (labeling with drag-assign + multi-select + sidebar toggle + name sort; cluster assign/remove/dissolve; person set-default/rename/delete/remove) - Tasks 3/4/5. The swappable data layer (`VidereClient` + `TauriClient`, components never call `invoke`) is Task 2, leaving `HttpClient` a future drop-in per the spec. Images via the Plan-2 protocols (Task 2 `FaceImage`). TanStack Query owns refetch/invalidation (Task 2), replacing manual reloads.
- **Placeholders:** foundational code (tooling, client, hooks, providers, routing, FaceImage) is complete and literal; the three view components are specified by behavior + exact command calls rather than full JSX, deliberately, because their pixel-level layout is parity-defined by the existing `*_HTML` in report.rs (the source of truth to read) and full transcription would be large - each task names the hooks, the commands, the state, and the acceptance reference.
- **Type/name consistency:** `invoke` arg keys are snake_case throughout (matching Plan 2's command signatures - the single highest-risk mismatch, called out explicitly in Task 2 Step 2); model field names match `videre-api`'s serde structs; route component exports match the imports in Task 6.
- **Risk:** the biggest runtime risk is a camelCase/snake_case arg mismatch silently passing `undefined` to a command (flagged in Task 2). Second is `BrowserRouter` history in a Tauri webview - if route navigation misbehaves, switch to `HashRouter` (same API).
