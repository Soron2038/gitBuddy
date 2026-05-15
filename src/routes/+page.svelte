<script lang="ts">
  import { onMount } from 'svelte';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { writeText } from '@tauri-apps/plugin-clipboard-manager';
  import { listen } from '@tauri-apps/api/event';
  import Buddy from '$lib/Buddy.svelte';
  import ContextMenu, { type MenuItem } from '$lib/ContextMenu.svelte';
  import {
    ghStatus,
    glStatus,
    cbStatus,
    listWaiting,
    listRepos,
    listReleases,
    listCi,
    listLocalRepos,
    getSettings,
    runEditor,
    indexLocalByRemote,
    localKeyForRepo,
    providerChipText,
    providerCssClass,
    type Viewer,
    type GitLabStatus,
    type CodebergStatus,
    type WaitingItem,
    type Repo,
    type LocalRepo,
    type Release,
    type CiRun,
    type CiStatus,
    type Settings,
  } from '$lib/data/api';

  // ── Auth state ────────────────────────────────────────────────────────
  let viewer: Viewer | null = $state(null);
  let gl: GitLabStatus | null = $state(null);
  let cb: CodebergStatus | null = $state(null);

  // ── Data ──────────────────────────────────────────────────────────────
  let items: WaitingItem[] = $state([]);
  let repos: Repo[] = $state([]);
  let locals: LocalRepo[] = $state([]);
  let releases: Release[] = $state([]);
  let ciRuns: CiRun[] = $state([]);
  let settings: Settings = $state({
    scan_roots: [],
    scan_ignore: [],
    gitlab_base_url: null,
    codeberg_base_url: null,
    editor_command: null,
    notifications_enabled: true,
  });

  // ── UI state ──────────────────────────────────────────────────────────
  let loading = $state(true);
  let refreshing = $state(false);
  let error: string | null = $state(null);
  let lastSyncedAt: Date | null = $state(null);

  // Context menu — shared instance, populated on right-click of any row.
  let menuOpen = $state(false);
  let menuX = $state(0);
  let menuY = $state(0);
  let menuItems: MenuItem[] = $state([]);

  // ── Derived ───────────────────────────────────────────────────────────
  let connected = $derived(viewer !== null || gl !== null || cb !== null);
  let displayName = $derived.by(() => {
    if (viewer) return viewer.name ?? viewer.login;
    if (gl) return gl.viewer.name ?? gl.viewer.login;
    if (cb) return cb.viewer.name ?? cb.viewer.login;
    return 'there';
  });

  let localByKey = $derived(indexLocalByRemote(locals));
  let ciByRepo = $derived(
    new Map(ciRuns.map((r) => [r.repo_id, r.status] as [string, CiStatus])),
  );

  // Stats grid — all derived from real data, no more hardcoded numbers.
  let waitingCount = $derived(items.length);
  let newReleasesCount = $derived(releases.filter((r) => r.is_new).length);
  let localCount = $derived(locals.length);
  let withUncommittedCount = $derived(
    locals.filter((l) => l.dirty_staged + l.dirty_unstaged + l.untracked > 0).length,
  );
  let ciPassingCount = $derived(ciRuns.filter((r) => r.status === 'ok').length);
  let ciTotalCount = $derived(ciRuns.length);
  let ciFailingCount = $derived(ciRuns.filter((r) => r.status === 'fail').length);
  let ciRunningCount = $derived(ciRuns.filter((r) => r.status === 'run').length);
  let providerCount = $derived(
    (viewer ? 1 : 0) + (gl ? 1 : 0) + (cb ? 1 : 0),
  );

  // Connected providers, shaped for the sidebar account list.
  type ProvBadge = {
    kind: 'github' | 'gitlab' | 'codeberg';
    viewer: Viewer;
    host: string;
  };
  let connectedProviders = $derived.by(() => {
    const out: ProvBadge[] = [];
    if (viewer) out.push({ kind: 'github', viewer, host: 'github.com' });
    if (gl) {
      try {
        out.push({ kind: 'gitlab', viewer: gl.viewer, host: new URL(gl.base_url).host });
      } catch {
        /* swallow malformed URL */
      }
    }
    if (cb) {
      try {
        out.push({ kind: 'codeberg', viewer: cb.viewer, host: new URL(cb.base_url).host });
      } catch {
        /* swallow malformed URL */
      }
    }
    return out;
  });

  function avatarClass(p: ProvBadge): string {
    if (p.kind === 'github') return 'gh-p';
    if (p.kind === 'gitlab') return p.host.includes('gitlab.com') ? 'gl-p' : 'gl-w';
    return 'cb';
  }
  function avatarText(p: ProvBadge): string {
    return p.viewer.login.charAt(0).toUpperCase();
  }
  /** Crude per-provider repo count for the sidebar. Falls back to host-
   *  matching for self-hosted GitLab where the `provider` tag is the
   *  generic `mpsd-gitlab` but the actual host varies per user. */
  function repoCountForProvider(p: ProvBadge): number {
    if (p.kind === 'github') return repos.filter((r) => r.provider === 'github').length;
    if (p.kind === 'codeberg') return repos.filter((r) => r.provider === 'codeberg').length;
    return repos.filter((r) => {
      try {
        return new URL(r.html_url).host === p.host;
      } catch {
        return false;
      }
    }).length;
  }

  // ── Data loading ──────────────────────────────────────────────────────
  async function loadAllData() {
    const [
      fetchedItems,
      fetchedRepos,
      fetchedReleases,
      fetchedCi,
      fetchedLocals,
      fetchedSettings,
    ] = await Promise.all([
      listWaiting().catch(() => []),
      listRepos().catch(() => []),
      listReleases().catch(() => []),
      listCi().catch(() => []),
      listLocalRepos().catch(() => []),
      getSettings().catch(() => settings),
    ]);
    items = fetchedItems;
    repos = fetchedRepos;
    releases = fetchedReleases;
    ciRuns = fetchedCi;
    locals = fetchedLocals;
    settings = fetchedSettings;
    lastSyncedAt = new Date();
  }

  /** Re-fetch auth status across all three providers. Used on mount and
   *  whenever the popover emits a `provider-changed` event after the user
   *  connects/disconnects an account. */
  async function refreshAuth() {
    const [ghViewer, glRes, cbRes] = await Promise.all([ghStatus(), glStatus(), cbStatus()]);
    viewer = ghViewer;
    gl = glRes;
    cb = cbRes;
  }

  onMount(() => {
    let cancelled = false;

    (async () => {
      try {
        await refreshAuth();
        if (connected) await loadAllData();
      } catch (e) {
        if (!cancelled) error = String(e);
      } finally {
        if (!cancelled) loading = false;
      }
    })();

    // Listen for provider connect/disconnect from the popover (or this
    // window's own actions, once the main window grows its own settings).
    const unlistenPromise = listen('provider-changed', async () => {
      try {
        await refreshAuth();
        if (connected) {
          await loadAllData();
        } else {
          items = [];
          repos = [];
          releases = [];
          ciRuns = [];
          locals = [];
        }
      } catch (e) {
        if (!cancelled) error = String(e);
      }
    });

    return () => {
      cancelled = true;
      void unlistenPromise.then((u) => u());
    };
  });

  async function refresh() {
    if (!connected || refreshing) return;
    refreshing = true;
    error = null;
    try {
      await loadAllData();
    } catch (e) {
      error = String(e);
    } finally {
      refreshing = false;
    }
  }

  // ── Live sync timer (1Hz tick so "Synced X sec ago" updates) ─────────
  let nowTick = $state(Date.now());
  $effect(() => {
    const handle = setInterval(() => (nowTick = Date.now()), 1000);
    return () => clearInterval(handle);
  });
  function humaniseSync(d: Date | null, _nowMs: number): string {
    if (!d) return 'never';
    const s = Math.max(0, Math.floor((Date.now() - d.getTime()) / 1000));
    if (s < 5) return 'just now';
    if (s < 60) return `${s} sec ago`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m} min ago`;
    return `${Math.floor(m / 60)}h ago`;
  }
  let syncText = $derived(humaniseSync(lastSyncedAt, nowTick));

  // ── Background polling ───────────────────────────────────────────────
  const POLL_INTERVAL_MS = 5 * 60 * 1000;
  $effect(() => {
    if (!connected) return;
    const handle = setInterval(() => void refresh(), POLL_INTERVAL_MS);
    return () => clearInterval(handle);
  });

  // ── Row actions ──────────────────────────────────────────────────────
  async function openExternal(url: string) {
    try {
      await openUrl(url);
    } catch {
      /* opener failure is non-fatal — clicking just doesn't navigate. */
    }
  }

  function openRepoMenu(e: MouseEvent, r: Repo) {
    e.preventDefault();
    const local = localByKey.get(localKeyForRepo(r));
    const localDiag = local?.[0];
    const m: MenuItem[] = [{ label: 'Open in browser', onclick: () => openExternal(r.html_url) }];
    if (localDiag) {
      m.push({ separator: true });
      m.push({ label: 'Show in Finder', onclick: () => revealItemInDir(localDiag.path) });
      const editorCmd = settings.editor_command?.trim() ?? '';
      if (editorCmd.length > 0) {
        m.push({ label: `Open in ${editorCmd}`, onclick: () => runEditor(localDiag.path) });
      }
    }
    if (r.clone_url || r.ssh_url) m.push({ separator: true });
    if (r.clone_url) {
      const url = r.clone_url;
      m.push({ label: 'Copy HTTPS clone URL', onclick: () => writeText(url) });
    }
    if (r.ssh_url) {
      const url = r.ssh_url;
      m.push({ label: 'Copy SSH clone URL', onclick: () => writeText(url) });
    }
    menuItems = m;
    menuX = e.clientX;
    menuY = e.clientY;
    menuOpen = true;
  }
</script>

<div class="app">

  <!-- Custom title bar (drag region). titleBarStyle: 'Overlay' on macOS
       means the traffic lights are drawn on top of this area at the left. -->
  <header class="titlebar" data-tauri-drag-region>
    <span class="tb-spacer" data-tauri-drag-region></span>
    <Buddy size={20} />
    <span class="brand" data-tauri-drag-region>gitBuddy</span>
    <span class="crumb" data-tauri-drag-region>/ <b>Overview</b></span>
    <span class="tb-flex" data-tauri-drag-region></span>
    <span class="sync">
      <span class="dot" aria-hidden="true"></span>
      {connected ? `Synced ${syncText}` : 'Not connected'}
    </span>
  </header>

  <!-- Toolbar with search + actions. Search is a placeholder in Phase 1;
       Phase 2 will wire it to a `$derived filteredRepos` against the real
       repo list. -->
  <div class="toolbar">
    <label class="search">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
        <circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" />
      </svg>
      <input type="text" placeholder="Search by repo, owner, label, anything…" disabled />
      <span class="sho">⌘ K</span>
    </label>
    <button
      class="iconbtn"
      class:spin={refreshing}
      data-tip="Refresh now"
      aria-label="Refresh"
      onclick={refresh}
      disabled={!connected || refreshing}
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
        <path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 4v5h-5" />
      </svg>
    </button>
    <button
      class="iconbtn"
      class:bell={waitingCount > 0}
      data-count={waitingCount}
      data-tip={waitingCount > 0 ? `${waitingCount} waiting` : 'Nothing waiting'}
      aria-label="Notifications"
    >
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M18 8a6 6 0 1 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" />
        <path d="M13.7 21a2 2 0 0 1-3.4 0"/>
      </svg>
    </button>
  </div>

  <div class="body">

    <!-- Sidebar -->
    <aside class="side">
      <section class="sec">
        <h3>What's <em>waiting</em></h3>
        <div class="pill on">
          <span class="sw t"></span> On you <span class="c">{waitingCount}</span>
        </div>
        <div class="pill">
          <span class="sw s"></span> All repos <span class="c">{repos.length}</span>
        </div>
        <div class="pill">
          <span class="sw b"></span> New releases <span class="c">{newReleasesCount}</span>
        </div>
        <div class="pill">
          <span class="sw p"></span> Local clones <span class="c">{localCount}</span>
        </div>
      </section>

      <section class="sec">
        <h3>Accounts</h3>
        {#if connectedProviders.length === 0}
          <p class="side-empty">No providers connected yet.</p>
        {:else}
          {#each connectedProviders as p (p.host)}
            <div class="pill">
              <span class="ava {avatarClass(p)}">{avatarText(p)}</span>
              <span class="acct-name">
                {p.viewer.login}
                <span class="acct-host">{p.host}</span>
              </span>
              <span class="c">{repoCountForProvider(p)}</span>
            </div>
          {/each}
        {/if}
      </section>
    </aside>

    <!-- Content -->
    <main class="content">
      {#if loading}
        <div class="empty-hero">
          <p class="empty-loading">Loading…</p>
        </div>
      {:else if !connected}
        <div class="empty-hero">
          <Buddy size={64} />
          <h2 class="empty-title">Connect a provider to get started.</h2>
          <p class="empty-sub">
            Click the gitBuddy icon in the menu bar to open the popover and
            add a GitHub, GitLab or Codeberg account. Tokens stay in your
            macOS Keychain.
          </p>
        </div>
      {:else}
        <div class="greet-row">
          <h1>Hi, <em>{displayName}</em>.</h1>
          <p class="lede">
            {waitingCount === 0 ? "You're all caught up" : `${waitingCount} ${waitingCount === 1 ? 'thing' : 'things'} need a look`}
            · {newReleasesCount} fresh {newReleasesCount === 1 ? 'release' : 'releases'}
            · {providerCount} {providerCount === 1 ? 'account' : 'accounts'}
          </p>
        </div>

        <div class="stats">
          <div class="stat t">
            <span class="lbl">Waiting on you</span>
            <span class="num">{waitingCount}</span>
            <span class="delta">
              {waitingCount === 0 ? 'caught up' : 'across all providers'}
            </span>
          </div>
          <div class="stat s">
            <span class="lbl">CI passing</span>
            <span class="num">
              {ciPassingCount}{#if ciTotalCount > 0}<em>/{ciTotalCount}</em>{/if}
            </span>
            <span class="delta">
              {ciFailingCount} failing · {ciRunningCount} running
            </span>
          </div>
          <div class="stat b">
            <span class="lbl">New releases</span>
            <span class="num">{newReleasesCount}</span>
            <span class="delta">in the last 7 days</span>
          </div>
          <div class="stat">
            <span class="lbl">Local clones</span>
            <span class="num">{localCount}</span>
            <span class="delta">
              {withUncommittedCount} with uncommitted
            </span>
          </div>
        </div>

        <h2 class="section-h">
          Your <em>repos</em>
          <span class="count">{repos.length} shown</span>
        </h2>

        {#if repos.length === 0}
          <p class="content-empty">
            None of your accounts surfaced any repos yet. If you just connected,
            give the first sync a moment — or hit Refresh.
          </p>
        {:else}
          <div class="repo-grid">
            {#each repos as r (r.id)}
              {@const local = localByKey.get(localKeyForRepo(r))}
              {@const localDiag = local?.[0]}
              {@const ci = ciByRepo.get(r.id) ?? 'none'}
              <button
                class="card"
                onclick={() => openExternal(r.html_url)}
                oncontextmenu={(e) => openRepoMenu(e, r)}
              >
                <span class="pchip {providerCssClass(r.provider)}">{providerChipText(r)}</span>
                <div class="rname">
                  <span class="owner">{r.owner}</span> / <b>{r.name}</b>
                  <div class="sub">
                    {#if local}
                      <span class="pin">
                        <span
                          class="d"
                          class:off={localDiag && (localDiag.dirty_staged + localDiag.dirty_unstaged + localDiag.untracked > 0 || localDiag.ahead > 0)}
                        ></span>
                        {localDiag?.path ?? 'cloned'}
                      </span>
                    {:else}
                      <span class="pin">
                        <span class="d off"></span> not cloned
                      </span>
                    {/if}
                    <span>{r.default_branch}</span>
                    {#if r.is_private}<span>private</span>{/if}
                    {#if r.is_fork}<span>fork</span>{/if}
                    {#if localDiag && (localDiag.dirty_staged + localDiag.dirty_unstaged > 0)}
                      <span class="warn">{localDiag.dirty_staged + localDiag.dirty_unstaged} uncommitted</span>
                    {/if}
                    {#if localDiag && localDiag.ahead > 0}
                      <span class="warn">{localDiag.ahead} unpushed</span>
                    {/if}
                  </div>
                </div>
                <div class="rmeta">
                  <span class="rci {ci}">
                    <span class="b"></span>
                    {#if ci === 'ok'}passing{:else if ci === 'fail'}failing{:else if ci === 'run'}running{:else if ci === 'cancelled'}cancelled{:else}no ci{/if}
                  </span>
                  {#if r.language}
                    <span class="lang">{r.language}</span>
                  {/if}
                  {#if r.stars > 0}
                    <span class="stars">★ {r.stars}</span>
                  {/if}
                </div>
              </button>
            {/each}
          </div>
        {/if}

        {#if error}
          <p class="err-banner">{error}</p>
        {/if}
      {/if}
    </main>
  </div>
</div>

<ContextMenu bind:open={menuOpen} x={menuX} y={menuY} items={menuItems} />

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    overflow: hidden;
  }

  /* Title bar ------------------------------------------------------- */
  .titlebar {
    height: 46px;
    background: linear-gradient(180deg, #FDF7EA 0%, #F4E9D2 100%);
    border-bottom: 1px solid var(--line);
    display: flex;
    align-items: center;
    padding: 0 18px;
    gap: 14px;
    -webkit-user-select: none;
    user-select: none;
  }
  .tb-spacer { width: 60px; }
  .tb-flex   { flex: 1; }
  .brand {
    font-family: var(--font-display);
    font-size: 16px;
    letter-spacing: -0.01em;
    color: var(--ink);
  }
  .crumb {
    font-size: 12.5px;
    color: var(--ink-3);
  }
  .crumb b { color: var(--ink); font-weight: 500; }
  .sync {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    background: var(--cream-2);
    padding: 4px 10px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .sync .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sage);
  }

  /* Toolbar -------------------------------------------------------- */
  .toolbar {
    padding: 16px 18px;
    display: flex;
    align-items: center;
    gap: 12px;
    border-bottom: 1px solid var(--line);
    background: var(--paper-2);
  }
  .search {
    flex: 1;
    height: 38px;
    background: var(--paper);
    border-radius: var(--r-md);
    border: 1px solid var(--line-2);
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 14px;
    color: var(--ink-3);
    font-size: 13.5px;
    box-shadow: var(--shadow-1);
  }
  .search input {
    flex: 1;
    border: 0;
    background: transparent;
    outline: none;
    font: inherit;
    color: var(--ink);
  }
  .search input::placeholder { color: var(--ink-3); }
  .search .sho {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    background: var(--cream-2);
    padding: 2px 7px;
    border-radius: 5px;
  }
  .iconbtn {
    width: 38px;
    height: 38px;
    border-radius: var(--r-md);
    display: grid;
    place-items: center;
    color: var(--ink-2);
    background: var(--paper);
    border: 1px solid var(--line-2);
    box-shadow: var(--shadow-1);
    position: relative;
  }
  .iconbtn:hover:not(:disabled) { background: var(--cream-2); }
  .iconbtn:disabled { opacity: 0.4; cursor: default; }
  .iconbtn.spin svg { animation: spin 0.9s linear infinite; }
  @keyframes spin { to { transform: rotate(360deg); } }

  .iconbtn.bell::after {
    content: attr(data-count);
    position: absolute;
    top: -4px;
    right: -4px;
    background: var(--terracotta);
    color: white;
    font-size: 9.5px;
    font-weight: 600;
    border-radius: 999px;
    padding: 2px 5px;
    box-shadow: 0 0 0 2px var(--paper-2);
    font-family: var(--font-mono);
    min-width: 8px;
    text-align: center;
  }

  /* Body ----------------------------------------------------------- */
  .body {
    flex: 1;
    display: grid;
    grid-template-columns: 240px 1fr;
    min-height: 0;
  }
  .side {
    padding: 22px 14px;
    border-right: 1px solid var(--line);
    background: var(--paper-2);
    overflow-y: auto;
  }
  .sec + .sec { margin-top: 20px; }
  .sec h3 {
    font-family: var(--font-display);
    font-size: 13.5px;
    color: var(--ink-2);
    padding: 0 8px 8px;
    margin: 0;
    letter-spacing: -0.005em;
    font-weight: 400;
  }
  .sec h3 em {
    font-style: italic;
    color: var(--terracotta);
  }
  .pill {
    width: 100%;
    padding: 9px 12px;
    border-radius: var(--r-md);
    display: flex;
    align-items: center;
    gap: 10px;
    color: var(--ink-2);
    font-size: 13.5px;
    text-align: left;
    margin-bottom: 2px;
  }
  .pill.on {
    background: var(--terracotta-soft);
    color: var(--ink);
    font-weight: 600;
  }
  .pill .c {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
  }
  .pill.on .c { color: var(--terracotta); }
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .sw.t { background: var(--terracotta); }
  .sw.s { background: var(--sage); }
  .sw.b { background: var(--butter); }
  .sw.p { background: var(--plum); }

  .acct-name {
    display: flex;
    flex-direction: column;
    line-height: 1.2;
    min-width: 0;
  }
  .acct-host {
    font-family: var(--font-mono);
    font-size: 10px;
    color: var(--ink-3);
    margin-top: 2px;
  }
  .side-empty {
    margin: 4px 8px 0;
    font-size: 12px;
    color: var(--ink-3);
    font-style: italic;
  }

  .ava {
    width: 22px;
    height: 22px;
    border-radius: var(--r-sm);
    display: grid;
    place-items: center;
    color: white;
    font-weight: 600;
    font-size: 11px;
    font-family: var(--font-display);
    flex-shrink: 0;
  }
  .ava.gh-p { background: linear-gradient(135deg, #6B5A4D, #2E211B); }
  .ava.gh-w { background: linear-gradient(135deg, #80987B, #4A5E48); }
  .ava.gl-p { background: linear-gradient(135deg, #E8A06A, #C66243); }
  .ava.gl-w { background: linear-gradient(135deg, #B6A5C9, #6E5E80); }
  .ava.cb   { background: linear-gradient(135deg, #8DBBC9, #4E7A8A); }

  /* Content -------------------------------------------------------- */
  .content {
    padding: 26px 30px 30px;
    overflow-y: auto;
    overflow-x: hidden;
  }

  /* Empty state when nothing's connected -------------------------- */
  .empty-hero {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    text-align: center;
    min-height: 60vh;
    gap: 14px;
    color: var(--ink-2);
  }
  .empty-loading {
    font-family: var(--font-display);
    font-style: italic;
    font-size: 18px;
    color: var(--ink-3);
    margin: 0;
  }
  .empty-title {
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 26px;
    letter-spacing: -0.02em;
    color: var(--ink);
    margin: 0;
  }
  .empty-sub {
    max-width: 380px;
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-3);
    line-height: 1.5;
  }
  .content-empty {
    margin: 20px 0;
    color: var(--ink-3);
    font-size: 13.5px;
  }
  .err-banner {
    margin-top: 20px;
    color: var(--plum);
    font-size: 12.5px;
    background: var(--plum-soft);
    padding: 8px 12px;
    border-radius: var(--r-sm);
  }

  .greet-row {
    display: flex;
    align-items: flex-end;
    gap: 14px;
    margin-bottom: 18px;
    flex-wrap: wrap;
  }
  .greet-row h1 {
    font-family: var(--font-display);
    font-size: 32px;
    letter-spacing: -0.02em;
    line-height: 1;
    color: var(--ink);
    margin: 0;
    font-weight: 400;
  }
  .greet-row h1 em {
    font-style: italic;
    color: var(--terracotta);
  }
  .greet-row .lede {
    margin: 0 0 6px;
    font-size: 13.5px;
    color: var(--ink-3);
  }

  .stats {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 12px;
    margin-bottom: 26px;
  }
  .stat {
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .stat .lbl { font-size: 11.5px; color: var(--ink-3); }
  .stat .num {
    font-family: var(--font-display);
    font-size: 34px;
    letter-spacing: -0.02em;
    line-height: 1;
    color: var(--ink);
  }
  .stat .num em { font-style: italic; font-size: 20px; opacity: 0.7; }
  .stat .delta {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    margin-top: 4px;
  }
  .stat.t {
    background: linear-gradient(135deg, #FBE6DA 0%, #F6D7C2 100%);
    border-color: rgba(198, 98, 67, 0.12);
  }
  .stat.t .num { color: var(--terracotta); }
  .stat.s {
    background: linear-gradient(135deg, #E7EDD9 0%, #DCE7CD 100%);
    border-color: rgba(128, 152, 123, 0.18);
  }
  .stat.s .num { color: #5E7758; }
  .stat.b {
    background: linear-gradient(135deg, #FBEED1 0%, #F4E0AE 100%);
    border-color: rgba(232, 185, 75, 0.2);
  }
  .stat.b .num { color: #B68C2C; }

  .section-h {
    font-family: var(--font-display);
    font-size: 20px;
    color: var(--ink);
    letter-spacing: -0.01em;
    margin: 0 0 12px;
    display: flex;
    align-items: baseline;
    font-weight: 400;
  }
  .section-h em {
    font-style: italic;
    color: var(--terracotta);
    margin-left: 4px;
  }
  .section-h .count {
    margin-left: 8px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
  }

  .repo-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
  .card {
    width: 100%;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 14px 16px;
    display: grid;
    grid-template-columns: 32px 1fr auto;
    gap: 12px;
    align-items: start;
    text-align: left;
    transition: transform 0.15s, box-shadow 0.15s;
    cursor: pointer;
  }
  .card:hover {
    transform: translateY(-1px);
    box-shadow: var(--shadow-2);
  }
  .pchip {
    width: 28px;
    height: 28px;
    border-radius: 9px;
    display: grid;
    place-items: center;
    background: var(--cream-2);
    color: var(--ink-2);
    font-weight: 700;
    font-size: 11px;
    font-family: var(--font-display);
  }
  .pchip.gh { background: #2E211B; color: #FAF4EA; }
  .pchip.gl { background: linear-gradient(135deg, #E89C5C, #C66243); color: white; }
  .pchip.cb { background: linear-gradient(135deg, #8DBBC9, #4E7A8A); color: white; }
  .pchip.gl-self { background: linear-gradient(135deg, #B6A5C9, #6E5E80); color: white; }

  .rname { line-height: 1.25; min-width: 0; }
  .rname .owner { color: var(--ink-3); font-weight: 400; font-size: 12.5px; }
  .rname b { font-weight: 600; font-size: 14.5px; color: var(--ink); }
  .rname .sub {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
    margin-top: 5px;
    font-size: 11.5px;
    color: var(--ink-3);
    font-family: var(--font-mono);
    letter-spacing: 0.01em;
  }
  .rname .sub .pin {
    display: inline-flex;
    gap: 4px;
    align-items: center;
    max-width: 200px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .rname .sub .pin .d {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--sage);
    flex-shrink: 0;
  }
  .rname .sub .pin .d.off {
    background: var(--ink-4);
    opacity: 0.5;
  }
  .rname .sub .warn { color: var(--terracotta); }
  .rmeta {
    text-align: right;
    display: flex;
    flex-direction: column;
    align-items: flex-end;
    gap: 5px;
    font-size: 12px;
    color: var(--ink-2);
    white-space: nowrap;
  }
  .rci {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-family: var(--font-mono);
  }
  .rci .b {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--sage);
  }
  .rci.fail { color: var(--terracotta); }
  .rci.fail .b { background: var(--terracotta); }
  .rci.run { color: #B68C2C; }
  .rci.run .b {
    background: var(--butter);
    animation: rci 1.4s ease-in-out infinite;
  }
  .rci.none { color: var(--ink-3); opacity: 0.5; }
  .rci.none .b { background: var(--ink-4); }
  .rci.cancelled { color: var(--ink-3); }
  .rci.cancelled .b { background: var(--ink-4); }
  @keyframes rci {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.5; transform: scale(0.8); }
  }
  .lang, .stars {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
  }

  /* Responsive collapse for narrow windows */
  @media (max-width: 720px) {
    .body { grid-template-columns: 1fr; }
    .side { display: none; }
    .stats { grid-template-columns: repeat(2, 1fr); }
    .repo-grid { grid-template-columns: 1fr; }
  }
</style>
