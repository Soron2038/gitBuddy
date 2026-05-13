# Main Window — Implementation Plan

> **Status:** drafted 2026-05-13, awaiting execution.
> **Why now:** the popover is a complete daily-driver, but the "Open main
> window" button opens a window full of M1 mockup data. The main window
> is the last big functional gap before OAuth (M6.3) and the v1 release
> polish (M7).

---

## Context (read this first if it's been a while)

The popover handles ambient checks: "what's waiting on me, what's new".
The PRD always envisaged a **separate main window** as the dense detail
surface — search across all repos, rich filters, per-repo detail view.

Right now that window exists and opens, but its content is still the
hand-built mockup data from M1. Everything around it (providers,
local index, releases, CI, notifications, settings) is real.

This plan brings the main window onto real data, then layers search +
filters + a detail view on top.

---

## Current state

### Lives where

- [`src/routes/+page.svelte`](../../src/routes/+page.svelte) — the main
  window route. Imports `repos`, `stats` from `$lib/data/stub.ts`.
- [`src-tauri/tauri.conf.json`](../../src-tauri/tauri.conf.json) — the
  `main` window: 1200×780, `visible: false`, `titleBarStyle: "Overlay"`.
- [`src-tauri/src/commands.rs`](../../src-tauri/src/commands.rs) —
  `open_main` command sets `ActivationPolicy::Regular` and shows the
  window. Mirrors the tray menu's "Open gitBuddy" item.

### What's already real elsewhere (just needs reuse)

| Need | Lives in |
|---|---|
| Auth + viewer status for 3 providers | `ghStatus`, `glStatus`, `cbStatus` in `src/lib/data/api.ts` |
| Aggregated waiting items | `listWaiting()` |
| Aggregated repo list | `listRepos()` |
| Releases / CI | `listReleases()` / `listCi()` |
| Local clone index | `listLocalRepos()` + `indexLocalByRemote()` + `localKeyForRepo()` |
| Provider chip text / CSS class | `providerChipText`, `providerCssClass`, `providerLabel` |
| Settings (read + write) | `getSettings`, `saveSettings` |
| Context menu component | `$lib/ContextMenu.svelte` |
| Buddy mascot | `$lib/Buddy.svelte` |

**No new Tauri commands needed.** The popover already exercises all the
data paths; the main window is a pure frontend refactor against the
existing API surface.

### What's stub today

In `src/routes/+page.svelte`:
- Imports `repos`, `stats` from `src/lib/data/stub.ts` — hardcoded data
- Sidebar account list: 5 hardcoded entries (bjoern.witt, mpsd-bw, etc.)
- Stats grid: literal numbers
- Repo cards: 8 hardcoded rows
- Top "Search…" input: no handler
- "View all →" link: no handler

---

## Open design decisions

Decide before writing code. Each comes with my recommendation in **bold**.

1. **Density**
   - *Question:* The mockup is fairly dense. With 100+ real repos, breathe more or stay dense?
   - **Stay dense.** That's the main window's raison d'être vs. the popover. Comfort lives in the popover.

2. **What does the sidebar filter actually filter?**
   - *Options:* (a) account toggles only, (b) account + status (Waiting / All / Releases / Local) + reason (Assigned / Review / Authored / Mentioned), (c) full search-query DSL
   - **Option (b).** Mockup-faithful. (c) is over-engineering for v1.

3. **Search scope**
   - *Options:* repos only · repos + items · everything
   - **Repos only for v1.** Repo name + owner, case-insensitive substring. Items + releases can come later if it feels missing.

4. **Click-on-repo behaviour**
   - *Options:* (a) open html_url in browser like popover does, (b) split-pane detail view inside the window
   - **Both, in two phases.** Phase 1 wires (a); Phase 3 adds (b). (a) ships value immediately without blocking on a layout decision.

5. **Window state on close**
   - *Options:* hide (macOS convention) vs. quit
   - **Hide.** Cmd+W closes to tray, app keeps running. Re-open via tray menu, popover button, or Cmd+Tab.

6. **Dock icon when main window is closed**
   - Right now `open_main` sets ActivationPolicy::Regular. When the user closes the main window, do we switch back to Accessory (dock icon hides) or stay Regular (icon stays)?
   - **Switch back to Accessory on main window close.** Matches the menu-bar app feel — dock icon is a "main window is open" indicator. Catch the `tauri::WindowEvent::CloseRequested` on the main window and toggle policy + `prevent_close()` so we hide rather than quit.

7. **Cross-window state sync (provider connect/disconnect)**
   - *Question:* When the user disconnects a provider in the popover settings, does the open main window get notified?
   - **Tauri event channel.** Emit `provider-changed` from the relevant commands; both windows subscribe and re-fetch. Cheaper than per-window polling and immediate.

---

## Implementation phases

Three commits, each shippable on its own.

### Phase 1 — Data wiring + window lifecycle (~½ day)

**Goal:** open the main window from the popover, see your actual repos, accounts, counts, with no stub data anywhere on the page.

Tasks:

1. Delete the `import { repos, stats, … } from '$lib/data/stub'` line. Stub file can stay on disk (used by the design-mockups still) but the main window stops referencing it.
2. Mirror the popover's data-loading pattern:
   - `onMount` fetches in parallel: `ghStatus`, `glStatus`, `cbStatus`, `listWaiting`, `listRepos`, `listCi`, `listReleases`, `listLocalRepos`, `getSettings`.
   - Same `connected = $derived(…)`, `displayName` etc.
3. Build the sidebar account list from connected providers (not hardcoded).
4. Stats grid: real numbers
   - Waiting: `items.length`
   - CI passing: from `ciRuns` — count `status === 'ok'` / total
   - New releases: from `releases.filter(r => r.is_new).length`
   - Local clones: `locals.length`
5. Repo cards: render from `repos`, with CI dot + local indicator (reuse `localKeyForRepo` + `indexLocalByRemote`).
6. Right-click on repo card → reuse `ContextMenu` from popover.
7. Click on repo card → `openUrl(r.html_url)` (in browser).
8. Empty state when no provider connected: simple "Open the popover to connect a provider" message. Don't duplicate the onboarding form here — it lives in the popover.
9. **Window lifecycle (Rust):**
   - On `WindowEvent::CloseRequested` for the `main` window, call `prevent_close()`, hide the window, and switch `ActivationPolicy` back to `Accessory`.
   - That's it — Cmd+W now hides; re-open from popover/tray brings it back.
10. **Provider-changed event:**
    - Emit `app.emit("provider-changed", ())` from `gh_set_token`, `gl_set_token`, `cb_set_token`, `gh_disconnect`, `gl_disconnect`, `cb_disconnect`.
    - Both `+page.svelte` and `popover/+page.svelte` subscribe via `@tauri-apps/api/event::listen("provider-changed", …)` and trigger their `refresh()`.

**Done when:**
- Open main window → no hardcoded names, all data is yours.
- Connect/disconnect in popover settings → main window updates within a poll cycle.
- Cmd+W on main window → window hides, dock icon disappears, popover still works.

**Commit message draft:**
`feat: wire main window to live providers + correct close behaviour`

---

### Phase 2 — Search + sidebar filters (~½ day)

**Goal:** the main window earns its existence as a "browse + filter" surface that the popover can't be.

Tasks:

1. **Search**
   - Top input becomes a `$state` bound value.
   - `filteredRepos = $derived(repos.filter(matchesSearch))`.
   - Substring match against `${owner}/${name}` and `description`, case-insensitive.
   - Empty input = no filter.
   - ⌘K focuses the input (Tauri global shortcut via `@tauri-apps/plugin-global-shortcut` — optional, can defer).

2. **Sidebar filters**
   - Status section ("What's *waiting*" in mockup): top-level filter that switches the main list between:
     - "On you" → render `items` (waiting items), not repos
     - "All repos" → render `filteredRepos`
     - "New releases" → render `releases.filter(r => r.is_new)`
     - "Local clones" → render `localOnly` (repos that have a local match)
   - The repo grid component (or a sibling) handles all 4 list types; reuse the existing repo-card layout where it makes sense.

3. **Account toggles**
   - Each connected account row gets a sage dot + click toggles its `enabled` state in a local Set.
   - `filteredRepos` further filters by enabled providers/accounts.
   - Click on account name → opens the provider's web home (`{base_url}/{login}`).

4. **Reason chips for the Waiting view**
   - Below the search bar, a small row of toggleable chips: Assigned / Review / Authored / Mentioned.
   - Default: all on. Off-state greys out the chip and filters `items` by `item.reason`.

5. **Performance**
   - Up to ~200 repos: just render all. Skip virtualization. If/when a user with 1000+ repos shows up, revisit.
   - Wrap the repo grid in a `<div class="virtual-window">` placeholder with `contain: layout style;` so we can drop in a virtualiser later without re-plumbing.

**Done when:**
- Search "claude" → repo list narrows to claude-containing names within ~16ms.
- Toggle off the work account → its repos vanish, count updates.
- Switch sidebar to "New releases" → grid swaps to release cards.
- All filters compose (search + account filter + status).

**Commit message draft:**
`feat(main): live search + sidebar filters + status switcher`

---

### Phase 3 — Repo detail pane (~1 day)

**Goal:** click a repo card → see everything about that repo in one place.

Tasks:

1. **Layout decision**: split-pane vs. modal.
   - Recommend split-pane: 60% list / 40% detail. Detail pane collapses when nothing selected.
   - Smooth slide-in via `transform` transition; no layout shift.

2. **Detail pane contents**, in display order:
   - **Header**: repo name (big), description, badges (private / fork / archived), open-in-browser link.
   - **Provider + clone block**: provider chip, full host, link to repo. If locally cloned: local path(s), branch, ahead/behind, dirty/untracked counts. If not cloned: "Not cloned" + "Clone…" button (defers to a later commit — clone command isn't built yet).
   - **CI**: status dot + last workflow run name + "View run" link.
   - **Latest release**: tag, name, age, NEW badge if recent, "View release" link.
   - **Waiting items in this repo**: filter `items` by `item.repo === full_name`, render the same row shape as the popover.

3. **Quick actions toolbar in the detail header**: same items as the right-click context menu (Open in browser, Show in Finder, Open in editor, Copy HTTPS/SSH).

4. **State management**: `selectedRepo = $state<Repo | null>(null)`. Click card → set. Escape or click outside → clear.

5. **Empty selection state**: show a friendly Buddy + "Pick a repo to see details".

**Done when:**
- Click a card → detail pane animates in, populated with real data.
- Local clone with dirty work → detail shows "3 uncommitted, 2 ahead" clearly.
- Escape → detail collapses, list reclaims the width.
- All quick actions work (clipboard, finder, editor).

**Commit message draft:**
`feat(main): per-repo detail pane with CI, releases, items, quick actions`

---

## Definition of done (overall)

After all 3 phases:

- [ ] No `import` of `$lib/data/stub` anywhere in `src/routes/+page.svelte`
- [ ] Open main window from popover → real data within 3 seconds
- [ ] Search + filters compose correctly
- [ ] Detail pane shows everything the user knows about a repo
- [ ] Provider connect/disconnect in popover → main window updates without restart
- [ ] Cmd+W hides instead of quits; dock icon flips correctly
- [ ] All checks green (svelte-check, cargo check, clippy -D warnings, fmt)
- [ ] Visual: still matches the Buddy design language (Gambarino headings, Switzer body, cream + terracotta + sage)

---

## Explicitly NOT in this work

- New Tauri commands beyond `emit("provider-changed")` calls in the existing auth commands
- Clone-from-detail-pane (needs a new `git_clone` command — separate commit)
- Cross-window keyboard shortcuts (separate, small)
- OAuth (M6.3)
- Releases + CI for non-GitHub providers
- Main window settings UI (settings stay in the popover — single source of truth)
- Multi-account per provider (still single-account-per-provider per M5)

---

## Risks / things to watch

1. **Popover and main window will share state via re-fetch on event, not via shared store.**
   Cheap and robust. Watch for double-fetches in close succession — debounce if needed.

2. **Polling cadence is per-window.**
   Both popover and main window poll every 5 min. That's 2× the API load when both are open. Acceptable for v1; consolidating later means moving polling into Rust.

3. **Dock-icon flap on window open/close.**
   ActivationPolicy changes are visible on macOS. Test that quickly opening/closing the main window doesn't strobe the dock icon.

4. **First-time empty state.**
   Phase 1 says "show a message linking to popover". Make sure that's not visually broken when nothing is connected — easy regression after wiring everything to real data.

5. **Performance with 200+ repos.**
   The repo card layout is heavy (multiple chips, gradients, hover transforms). Profile early; if scroll jank shows, swap to a simpler row.

---

## Quick mental refresher when picking this up later

1. Open `src/routes/popover/+page.svelte` — that's the working template for "real data wiring against the API". Phase 1 is largely "do that, but in `+page.svelte` with the main-window markup".
2. The api.ts surface is complete — you don't need to write any new commands or types for Phases 1+2.
3. Phase 3 might want one helper: `itemsForRepo(repo)` derived state. Otherwise still pure frontend.
4. Don't rebuild settings in the main window. Forward to the popover's settings panel (open popover via tray) — single source of truth.

— end —
