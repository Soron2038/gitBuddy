<script lang="ts">
  import { onMount } from 'svelte';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { writeText } from '@tauri-apps/plugin-clipboard-manager';
  import { listen } from '@tauri-apps/api/event';
  import Buddy from '$lib/Buddy.svelte';
  import ContextMenu, { type MenuItem } from '$lib/ContextMenu.svelte';
  import {
    ghStatus,
    ghSetToken,
    ghDisconnect,
    glStatus,
    glSetToken,
    glDisconnect,
    cbStatus,
    cbSetToken,
    cbDisconnect,
    listWaiting,
    listRepos,
    listReleases,
    listCi,
    listLocalRepos,
    getSettings,
    saveSettings,
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

  type View = 'overview' | 'settings';

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
  let view: View = $state('overview');
  let loading = $state(true);
  let refreshing = $state(false);
  let error: string | null = $state(null);
  let lastSyncedAt: Date | null = $state(null);

  // Settings-form state (editable mirrors of `settings`).
  let savingSettings = $state(false);
  let editorInput = $state('');
  $effect(() => {
    editorInput = settings.editor_command ?? '';
  });

  // Add-Provider state (only used inside the Settings view).
  let addingProvider = $state(false);
  let chosenProvider: 'github' | 'gitlab' | 'codeberg' = $state('github');
  let tokenInput = $state('');
  let gitlabBaseInput = $state('https://gitlab.com');
  let codebergBaseInput = $state('https://codeberg.org');
  let connecting = $state(false);

  // Context menu (right-click on repo cards).
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
  let providerCount = $derived((viewer ? 1 : 0) + (gl ? 1 : 0) + (cb ? 1 : 0));

  let canAddGithub = $derived(viewer === null);
  let canAddGitlab = $derived(gl === null);
  let canAddCodeberg = $derived(cb === null);
  let canAddAny = $derived(canAddGithub || canAddGitlab || canAddCodeberg);
  let availableProviderTabs = $derived(
    [
      canAddGithub && 'github',
      canAddGitlab && 'gitlab',
      canAddCodeberg && 'codeberg',
    ].filter(Boolean) as Array<'github' | 'gitlab' | 'codeberg'>,
  );

  /** Hosts seen in local orphan clones, filtered by which provider tab the
   *  user is in. Drives the quick-pick chips for GitLab and Codeberg
   *  onboarding so self-hosted hostnames don't have to be retyped. */
  function hostSuggestionsFor(target: 'gitlab' | 'codeberg'): string[] {
    const out = new Set<string>();
    for (const o of locals) {
      const h = o.remote?.host;
      if (!h) continue;
      if (h === 'github.com') continue;
      if (gl && gl.base_url.includes(h)) continue;
      if (cb && cb.base_url.includes(h)) continue;
      const isGitlabLike = h.includes('gitlab');
      if (target === 'gitlab' && !isGitlabLike && out.size > 0) continue;
      if (target === 'codeberg' && isGitlabLike) continue;
      out.add(h);
    }
    return Array.from(out).sort();
  }
  let gitlabHostSuggestions = $derived(hostSuggestionsFor('gitlab'));
  let codebergHostSuggestions = $derived(hostSuggestionsFor('codeberg'));

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
        /* malformed URL */
      }
    }
    if (cb) {
      try {
        out.push({ kind: 'codeberg', viewer: cb.viewer, host: new URL(cb.base_url).host });
      } catch {
        /* malformed URL */
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
        else {
          // Even with no provider connected we want the local scan + settings
          // so the Settings view's host suggestions and persisted config are
          // ready as soon as the user navigates over.
          const [fetchedLocals, fetchedSettings] = await Promise.all([
            listLocalRepos().catch(() => []),
            getSettings().catch(() => settings),
          ]);
          locals = fetchedLocals;
          settings = fetchedSettings;
        }
      } catch (e) {
        if (!cancelled) error = String(e);
      } finally {
        if (!cancelled) loading = false;
      }
    })();

    // Provider connect/disconnect from anywhere → refresh auth + data.
    const unlistenProviderPromise = listen('provider-changed', async () => {
      try {
        await refreshAuth();
        if (connected) {
          await loadAllData();
        } else {
          items = [];
          repos = [];
          releases = [];
          ciRuns = [];
          // Keep `locals` — local scan doesn't depend on provider auth.
        }
      } catch (e) {
        if (!cancelled) error = String(e);
      }
    });

    // Settings changed from anywhere → re-load. Cheap (small JSON file).
    const unlistenSettingsPromise = listen('settings-changed', async () => {
      try {
        settings = await getSettings();
      } catch (e) {
        if (!cancelled) error = String(e);
      }
    });

    // Popover's gear icon emits this with payload 'settings'. We just trust
    // any incoming payload — the only navigation target right now is settings.
    const unlistenNavPromise = listen<string>('main-window-navigate', (e) => {
      if (e.payload === 'settings') {
        view = 'settings';
      } else if (e.payload === 'overview') {
        view = 'overview';
      }
    });

    return () => {
      cancelled = true;
      void unlistenProviderPromise.then((u) => u());
      void unlistenSettingsPromise.then((u) => u());
      void unlistenNavPromise.then((u) => u());
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

  // ── Live sync timer ──────────────────────────────────────────────────
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

  // ── Polling ──────────────────────────────────────────────────────────
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
      /* swallow */
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

  // ── Settings actions ─────────────────────────────────────────────────
  async function addScanRoot() {
    let chosen: string | null = null;
    try {
      const result = await openDialog({
        directory: true,
        multiple: false,
        title: 'Choose a folder to scan for Git repositories',
      });
      if (typeof result === 'string') chosen = result;
    } catch (e) {
      error = `Folder picker failed: ${e}`;
      return;
    }
    if (!chosen) return;
    if (settings.scan_roots.includes(chosen)) return;
    settings = { ...settings, scan_roots: [...settings.scan_roots, chosen] };
    await persistSettings();
    await rescanLocals();
  }

  async function removeScanRoot(path: string) {
    settings = {
      ...settings,
      scan_roots: settings.scan_roots.filter((p) => p !== path),
    };
    await persistSettings();
    await rescanLocals();
  }

  async function persistSettings() {
    savingSettings = true;
    try {
      await saveSettings(settings);
    } catch (e) {
      error = `Saving settings failed: ${e}`;
    } finally {
      savingSettings = false;
    }
  }

  async function rescanLocals() {
    try {
      locals = await listLocalRepos();
    } catch (e) {
      error = `Local scan failed: ${e}`;
    }
  }

  async function persistEditorCommand() {
    const next = editorInput.trim();
    const normalised = next.length === 0 ? null : next;
    if (normalised === (settings.editor_command ?? null)) return;
    settings = { ...settings, editor_command: normalised };
    await persistSettings();
  }

  async function toggleNotifications(value: boolean) {
    if (value === settings.notifications_enabled) return;
    settings = { ...settings, notifications_enabled: value };
    await persistSettings();
  }

  async function disconnect(kind: 'github' | 'gitlab' | 'codeberg') {
    const label =
      kind === 'github' ? 'GitHub' : kind === 'gitlab' ? 'GitLab' : 'Codeberg';
    if (!confirm(`Disconnect ${label}? The stored token will be removed from your Keychain.`)) {
      return;
    }
    error = null;
    try {
      if (kind === 'github') {
        await ghDisconnect();
        viewer = null;
      } else if (kind === 'gitlab') {
        await glDisconnect();
        gl = null;
      } else {
        await cbDisconnect();
        cb = null;
      }
      // The provider-changed event listener will re-fetch data; nothing to do
      // here. But we explicitly null the local copy of the disconnected
      // provider for the UI to update instantly without waiting for the
      // event to bounce back.
    } catch (e) {
      error = String(e);
    }
  }

  function startAddingProvider() {
    addingProvider = true;
    tokenInput = '';
    error = null;
    if (availableProviderTabs.length === 1) {
      chosenProvider = availableProviderTabs[0];
    } else if (!availableProviderTabs.includes(chosenProvider)) {
      chosenProvider = availableProviderTabs[0] ?? 'github';
    }
  }

  function cancelAddingProvider() {
    addingProvider = false;
    tokenInput = '';
    error = null;
  }

  async function connectProvider() {
    if (!tokenInput.trim()) return;
    if (chosenProvider === 'gitlab' && !gitlabBaseInput.trim()) return;
    if (chosenProvider === 'codeberg' && !codebergBaseInput.trim()) return;
    connecting = true;
    error = null;
    try {
      if (chosenProvider === 'github') {
        viewer = await ghSetToken(tokenInput.trim());
      } else if (chosenProvider === 'gitlab') {
        await glSetToken(tokenInput.trim(), gitlabBaseInput.trim());
        gl = await glStatus();
      } else {
        await cbSetToken(tokenInput.trim(), codebergBaseInput.trim());
        cb = await cbStatus();
      }
      tokenInput = '';
      addingProvider = false;
      // Trigger a data reload so the freshly connected provider's repos
      // appear immediately when the user returns to the overview.
      await loadAllData();
    } catch (e) {
      error = String(e);
    } finally {
      connecting = false;
    }
  }
</script>

<div class="app">

  <!-- Custom title bar (drag region). titleBarStyle: 'Overlay' on macOS
       means the traffic lights are drawn on top of this area at the left. -->
  <header class="titlebar" data-tauri-drag-region>
    <span class="tb-spacer" data-tauri-drag-region></span>
    <Buddy size={20} />
    <span class="brand" data-tauri-drag-region>gitBuddy</span>
    {#if view === 'settings'}
      <button
        type="button"
        class="back"
        data-tip="Back to overview"
        aria-label="Back to overview"
        onclick={() => (view = 'overview')}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
          <path d="M19 12H5" /><path d="m12 19-7-7 7-7" />
        </svg>
        Overview
      </button>
      <span class="crumb" data-tauri-drag-region>/ <b>Settings</b></span>
    {:else}
      <span class="crumb" data-tauri-drag-region>/ <b>Overview</b></span>
    {/if}
    <span class="tb-flex" data-tauri-drag-region></span>
    <span class="sync">
      <span class="dot" aria-hidden="true"></span>
      {connected ? `Synced ${syncText}` : 'Not connected'}
    </span>
  </header>

  {#if view === 'overview'}
    <!-- ─────────── Overview ─────────── -->
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
      <button
        class="iconbtn"
        data-tip="Settings"
        aria-label="Open settings"
        onclick={() => (view = 'settings')}
      >
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
        </svg>
      </button>
    </div>

    <div class="body">
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
              Open the popover from the menu bar and connect a GitHub, GitLab,
              or Codeberg account — or jump straight into
              <button type="button" class="link-inline" onclick={() => (view = 'settings')}>
                Settings
              </button>
              from here.
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
  {:else}
    <!-- ─────────── Settings ─────────── -->
    <main class="settings">
      <div class="settings-inner">
        <h1 class="settings-title">Settings</h1>

        <!-- Connected providers -->
        <section class="set-sec">
          <h3>Connected <em>providers</em></h3>
          {#if !connected}
            <p class="set-empty">No providers yet — add one below.</p>
          {:else}
            <ul class="prov-list">
              {#if viewer}
                <li class="prov-row">
                  <span class="pchip gh">gh</span>
                  <div class="prov-meta">
                    <div class="prov-name">{viewer.name ?? viewer.login}</div>
                    <div class="prov-host">github.com</div>
                  </div>
                  <button
                    type="button"
                    class="prov-disconnect"
                    onclick={() => disconnect('github')}
                  >
                    Disconnect
                  </button>
                </li>
              {/if}
              {#if gl}
                <li class="prov-row">
                  <span class="pchip {gl.base_url.includes('gitlab.com') ? 'gl' : 'gl-self'}">
                    {gl.base_url.includes('gitlab.com')
                      ? 'gl'
                      : providerChipText({ provider: 'mpsd-gitlab', html_url: gl.base_url })}
                  </span>
                  <div class="prov-meta">
                    <div class="prov-name">{gl.viewer.name ?? gl.viewer.login}</div>
                    <div class="prov-host">{new URL(gl.base_url).host}</div>
                  </div>
                  <button
                    type="button"
                    class="prov-disconnect"
                    onclick={() => disconnect('gitlab')}
                  >
                    Disconnect
                  </button>
                </li>
              {/if}
              {#if cb}
                <li class="prov-row">
                  <span class="pchip cb">cb</span>
                  <div class="prov-meta">
                    <div class="prov-name">{cb.viewer.name ?? cb.viewer.login}</div>
                    <div class="prov-host">{new URL(cb.base_url).host}</div>
                  </div>
                  <button
                    type="button"
                    class="prov-disconnect"
                    onclick={() => disconnect('codeberg')}
                  >
                    Disconnect
                  </button>
                </li>
              {/if}
            </ul>
          {/if}

          {#if canAddAny && !addingProvider}
            <button type="button" class="set-add" onclick={startAddingProvider}>
              + Add provider…
            </button>
          {/if}

          {#if addingProvider}
            <div class="add-provider">
              {#if availableProviderTabs.length > 1}
                <div class="provider-tabs">
                  {#each availableProviderTabs as p}
                    <button
                      type="button"
                      class:on={chosenProvider === p}
                      onclick={() => (chosenProvider = p)}
                    >
                      {p === 'github' ? 'GitHub' : p === 'gitlab' ? 'GitLab' : 'Codeberg'}
                    </button>
                  {/each}
                </div>
              {/if}

              {#if chosenProvider === 'github' && canAddGithub}
                <button
                  type="button"
                  class="token-link"
                  onclick={() =>
                    openExternal(
                      'https://github.com/settings/tokens/new?description=gitBuddy&scopes=repo,read:org',
                    )}
                >
                  Create a token on GitHub →
                </button>
                <label class="token-input">
                  <span class="lbl">Personal access token</span>
                  <input
                    type="password"
                    placeholder="ghp_… or github_pat_…"
                    bind:value={tokenInput}
                    onkeydown={(e) => e.key === 'Enter' && connectProvider()}
                    disabled={connecting}
                    autocomplete="off"
                    spellcheck="false"
                  />
                </label>
              {:else if chosenProvider === 'gitlab' && canAddGitlab}
                <label class="token-input">
                  <span class="lbl">Instance URL</span>
                  <input
                    type="url"
                    placeholder="https://gitlab.com"
                    bind:value={gitlabBaseInput}
                    disabled={connecting}
                    autocomplete="off"
                    spellcheck="false"
                  />
                </label>
                {#if gitlabHostSuggestions.length > 0}
                  <div class="host-hints">
                    <span class="hint">Found in your local clones:</span>
                    <div class="host-chips">
                      {#each gitlabHostSuggestions as host}
                        <button
                          type="button"
                          class="host-chip"
                          onclick={() => (gitlabBaseInput = `https://${host}`)}
                        >
                          {host}
                        </button>
                      {/each}
                    </div>
                  </div>
                {/if}
                <button
                  type="button"
                  class="token-link"
                  onclick={() =>
                    openExternal(
                      `${gitlabBaseInput.replace(/\/$/, '')}/-/user_settings/personal_access_tokens?name=gitBuddy&scopes=api,read_user`,
                    )}
                >
                  Create a token on this GitLab →
                </button>
                <label class="token-input">
                  <span class="lbl">Personal access token</span>
                  <input
                    type="password"
                    placeholder="glpat-…"
                    bind:value={tokenInput}
                    onkeydown={(e) => e.key === 'Enter' && connectProvider()}
                    disabled={connecting}
                    autocomplete="off"
                    spellcheck="false"
                  />
                </label>
              {:else if chosenProvider === 'codeberg' && canAddCodeberg}
                <label class="token-input">
                  <span class="lbl">Instance URL</span>
                  <input
                    type="url"
                    placeholder="https://codeberg.org"
                    bind:value={codebergBaseInput}
                    disabled={connecting}
                    autocomplete="off"
                    spellcheck="false"
                  />
                </label>
                {#if codebergHostSuggestions.length > 0}
                  <div class="host-hints">
                    <span class="hint">Found in your local clones:</span>
                    <div class="host-chips">
                      {#each codebergHostSuggestions as host}
                        <button
                          type="button"
                          class="host-chip"
                          onclick={() => (codebergBaseInput = `https://${host}`)}
                        >
                          {host}
                        </button>
                      {/each}
                    </div>
                  </div>
                {/if}
                <button
                  type="button"
                  class="token-link"
                  onclick={() =>
                    openExternal(
                      `${codebergBaseInput.replace(/\/$/, '')}/user/settings/applications`,
                    )}
                >
                  Create a token on this Gitea/Forgejo →
                </button>
                <label class="token-input">
                  <span class="lbl">Personal access token</span>
                  <input
                    type="password"
                    placeholder="token"
                    bind:value={tokenInput}
                    onkeydown={(e) => e.key === 'Enter' && connectProvider()}
                    disabled={connecting}
                    autocomplete="off"
                    spellcheck="false"
                  />
                </label>
              {/if}

              {#if error}
                <p class="err">{error}</p>
              {/if}

              <div class="setup-actions">
                <button
                  type="button"
                  class="secondary"
                  onclick={cancelAddingProvider}
                  disabled={connecting}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  class="primary"
                  onclick={connectProvider}
                  disabled={connecting || !tokenInput.trim() || (chosenProvider === 'gitlab' && !gitlabBaseInput.trim()) || (chosenProvider === 'codeberg' && !codebergBaseInput.trim())}
                >
                  {connecting ? 'Verifying…' : 'Connect'}
                </button>
              </div>
            </div>
          {/if}
        </section>

        <!-- Scan roots -->
        <section class="set-sec">
          <h3>Scan <em>roots</em></h3>
          <p class="set-help">
            gitBuddy walks these folders looking for <code>.git</code> checkouts.
            <code>node_modules</code>, build outputs and macOS junk are skipped.
          </p>
          {#if settings.scan_roots.length === 0}
            <p class="set-empty">No scan roots yet.</p>
          {:else}
            <ul class="path-list">
              {#each settings.scan_roots as path (path)}
                <li class="path-row">
                  <span class="path-text" title={path}>{path}</span>
                  <button
                    type="button"
                    class="path-remove"
                    data-tip="Remove from scan list"
                    aria-label="Remove {path}"
                    onclick={() => removeScanRoot(path)}
                    disabled={savingSettings}
                  >
                    ×
                  </button>
                </li>
              {/each}
            </ul>
          {/if}
          <button
            type="button"
            class="set-add"
            onclick={addScanRoot}
            disabled={savingSettings}
          >
            + Add folder…
          </button>
        </section>

        <!-- Open-in-editor command -->
        <section class="set-sec">
          <h3>Open in <em>editor</em></h3>
          <p class="set-help">
            Command run when you pick <em>Open in editor</em> from a repo's
            right-click menu. The repo's local path is appended. Common
            values: <code>code</code>, <code>cursor</code>, <code>zed</code>,
            <code>idea</code>. Leave empty to hide that menu entry.
          </p>
          <input
            type="text"
            class="set-input"
            bind:value={editorInput}
            onblur={persistEditorCommand}
            onkeydown={(e) => e.key === 'Enter' && persistEditorCommand()}
            placeholder="code"
            spellcheck="false"
            autocomplete="off"
          />
        </section>

        <!-- Notifications -->
        <section class="set-sec">
          <h3><em>Notifications</em></h3>
          <p class="set-help">
            Fire a macOS notification when a poll surfaces a new issue, PR
            or MR that's waiting on you. Releases and CI events join in a
            later iteration. The actual permission is controlled by macOS —
            check <em>System Settings → Notifications → gitBuddy</em> if
            nothing shows up despite this being on.
          </p>
          <label class="set-toggle">
            <input
              type="checkbox"
              checked={settings.notifications_enabled}
              onchange={(e) => toggleNotifications((e.target as HTMLInputElement).checked)}
            />
            <span>Enable notifications</span>
          </label>
        </section>

        {#if error && !addingProvider}
          <p class="err-banner">{error}</p>
        {/if}
      </div>
    </main>
  {/if}
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
  .back {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    padding: 4px 10px 4px 8px;
    border-radius: var(--r-sm);
    color: var(--ink-2);
    background: transparent;
    border: 1px solid transparent;
    font-size: 12.5px;
    cursor: pointer;
    -webkit-app-region: no-drag;
  }
  .back:hover {
    background: var(--cream-2);
    border-color: var(--line);
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

  /* Overview body ------------------------------------------------- */
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

  /* Overview content ---------------------------------------------- */
  .content {
    padding: 26px 30px 30px;
    overflow-y: auto;
    overflow-x: hidden;
  }
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
    max-width: 440px;
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-3);
    line-height: 1.5;
  }
  .link-inline {
    color: var(--terracotta);
    background: transparent;
    border: 0;
    padding: 0;
    font: inherit;
    cursor: pointer;
    text-decoration: underline;
    text-underline-offset: 2px;
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

  /* Settings view -------------------------------------------------- */
  .settings {
    flex: 1;
    overflow-y: auto;
    background: var(--paper);
  }
  .settings-inner {
    max-width: 640px;
    margin: 0 auto;
    padding: 36px 32px 60px;
  }
  .settings-title {
    font-family: var(--font-display);
    font-size: 36px;
    font-weight: 400;
    letter-spacing: -0.02em;
    color: var(--ink);
    margin: 0 0 24px;
  }
  .set-sec {
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 22px 24px;
    margin-bottom: 18px;
  }
  .set-sec h3 {
    font-family: var(--font-display);
    font-size: 18px;
    font-weight: 400;
    letter-spacing: -0.01em;
    color: var(--ink);
    margin: 0 0 6px;
  }
  .set-sec h3 em {
    font-style: italic;
    color: var(--terracotta);
  }
  .set-help {
    margin: 0 0 14px;
    font-size: 12.5px;
    color: var(--ink-3);
    line-height: 1.5;
  }
  .set-help code {
    font-family: var(--font-mono);
    background: var(--cream-2);
    padding: 1px 5px;
    border-radius: 4px;
    font-size: 11.5px;
    color: var(--ink-2);
  }
  .set-help em {
    font-style: italic;
    color: var(--ink-2);
  }
  .set-empty {
    margin: 0 0 12px;
    color: var(--ink-3);
    font-size: 13px;
    font-style: italic;
  }
  .set-add {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 32px;
    padding: 0 14px;
    background: var(--paper);
    border: 1px dashed var(--line-2);
    border-radius: var(--r-sm);
    font-size: 12.5px;
    color: var(--terracotta);
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
  }
  .set-add:hover:not(:disabled) {
    background: var(--cream-2);
    border-color: var(--terracotta);
  }
  .set-add:disabled { opacity: 0.5; cursor: default; }
  .set-input {
    width: 100%;
    height: 36px;
    padding: 0 12px;
    border: 1px solid var(--line-2);
    border-radius: var(--r-sm);
    font: inherit;
    font-family: var(--font-mono);
    font-size: 13px;
    background: var(--paper);
    color: var(--ink);
    outline: none;
    transition: border-color 0.15s, background 0.15s;
  }
  .set-input:focus {
    border-color: var(--terracotta);
    background: var(--paper);
  }
  .set-toggle {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    font-size: 13.5px;
    color: var(--ink);
    cursor: pointer;
    user-select: none;
  }
  .set-toggle input[type='checkbox'] {
    appearance: none;
    width: 34px;
    height: 20px;
    border-radius: 999px;
    background: var(--cream-3);
    position: relative;
    cursor: pointer;
    transition: background 0.15s;
    flex-shrink: 0;
  }
  .set-toggle input[type='checkbox']::after {
    content: '';
    position: absolute;
    top: 2px;
    left: 2px;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--paper);
    transition: transform 0.18s ease;
    box-shadow: 0 1px 2px rgba(0, 0, 0, 0.18);
  }
  .set-toggle input[type='checkbox']:checked {
    background: var(--sage);
  }
  .set-toggle input[type='checkbox']:checked::after {
    transform: translateX(14px);
  }

  .prov-list {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .prov-row {
    display: grid;
    grid-template-columns: 32px 1fr auto;
    gap: 14px;
    align-items: center;
    padding: 10px 12px;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
  }
  .prov-meta { line-height: 1.25; min-width: 0; }
  .prov-name {
    font-weight: 500;
    color: var(--ink);
    font-size: 13.5px;
  }
  .prov-host {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
    margin-top: 2px;
  }
  .prov-disconnect {
    height: 28px;
    padding: 0 12px;
    font-size: 12px;
    color: var(--ink-3);
    background: transparent;
    border: 1px solid var(--line-2);
    border-radius: var(--r-sm);
    cursor: pointer;
    transition: color 0.15s, background 0.15s, border-color 0.15s;
  }
  .prov-disconnect:hover {
    color: var(--plum);
    background: var(--plum-soft);
    border-color: transparent;
  }

  .path-list {
    list-style: none;
    margin: 0 0 12px;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .path-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
  }
  .path-text {
    flex: 1;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--ink-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path-remove {
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    background: transparent;
    border: 0;
    color: var(--ink-3);
    font-size: 16px;
    line-height: 1;
    cursor: pointer;
  }
  .path-remove:hover:not(:disabled) {
    background: var(--plum-soft);
    color: var(--plum);
  }
  .path-remove:disabled { opacity: 0.4; cursor: default; }

  /* Add-Provider inline panel ------------------------------------- */
  .add-provider {
    margin-top: 14px;
    padding: 16px;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--r-md);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .provider-tabs {
    display: flex;
    gap: 4px;
    padding: 4px;
    background: var(--cream-2);
    border-radius: var(--r-sm);
    font-size: 12.5px;
  }
  .provider-tabs button {
    flex: 1;
    padding: 6px 10px;
    color: var(--ink-2);
    border-radius: 6px;
    background: transparent;
    border: 0;
    cursor: pointer;
  }
  .provider-tabs button.on {
    background: var(--paper);
    color: var(--ink);
    font-weight: 600;
    box-shadow: var(--shadow-1);
  }
  .token-link {
    align-self: flex-start;
    color: var(--terracotta);
    font-size: 12.5px;
    background: transparent;
    border: 0;
    padding: 0;
    cursor: pointer;
    text-decoration: none;
  }
  .token-link:hover { text-decoration: underline; }
  .token-input {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .token-input .lbl {
    font-size: 11px;
    color: var(--ink-3);
    font-family: var(--font-mono);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }
  .token-input input {
    height: 36px;
    padding: 0 12px;
    border: 1px solid var(--line-2);
    border-radius: var(--r-sm);
    font: inherit;
    font-family: var(--font-mono);
    font-size: 12.5px;
    background: var(--paper-2);
    color: var(--ink);
    outline: none;
    transition: border-color 0.15s, background 0.15s;
  }
  .token-input input:focus {
    border-color: var(--terracotta);
    background: var(--paper);
  }
  .host-hints {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .host-hints .hint {
    font-size: 11.5px;
    color: var(--ink-3);
  }
  .host-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .host-chip {
    height: 24px;
    padding: 0 10px;
    background: var(--cream-2);
    border: 1px solid transparent;
    border-radius: 999px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-2);
    cursor: pointer;
    transition: background 0.15s, border-color 0.15s;
  }
  .host-chip:hover {
    background: var(--paper);
    border-color: var(--terracotta);
    color: var(--terracotta);
  }
  .setup-actions {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
  }
  .primary {
    height: 36px;
    padding: 0 16px;
    background: var(--terracotta);
    color: var(--paper);
    border: 0;
    border-radius: var(--r-sm);
    font-weight: 600;
    font-size: 13px;
    cursor: pointer;
    transition: background 0.15s, opacity 0.15s;
  }
  .primary:hover:not(:disabled) { background: #B05738; }
  .primary:disabled { opacity: 0.5; cursor: default; }
  .secondary {
    height: 36px;
    padding: 0 14px;
    background: transparent;
    border: 1px solid var(--line-2);
    color: var(--ink-2);
    border-radius: var(--r-sm);
    font-size: 13px;
    cursor: pointer;
  }
  .secondary:hover:not(:disabled) { background: var(--cream-2); }
  .secondary:disabled { opacity: 0.5; cursor: default; }
  .err {
    margin: 0;
    color: var(--plum);
    font-size: 12.5px;
    background: var(--plum-soft);
    padding: 8px 10px;
    border-radius: var(--r-sm);
  }

  /* Responsive collapse for narrow windows */
  @media (max-width: 720px) {
    .body { grid-template-columns: 1fr; }
    .side { display: none; }
    .stats { grid-template-columns: repeat(2, 1fr); }
    .repo-grid { grid-template-columns: 1fr; }
  }
</style>
