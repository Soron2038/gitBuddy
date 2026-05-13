<script lang="ts">
  import { onMount } from 'svelte';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { writeText } from '@tauri-apps/plugin-clipboard-manager';
  import Buddy from '$lib/Buddy.svelte';
  import ContextMenu, { type MenuItem } from '$lib/ContextMenu.svelte';
  import {
    ghStatus,
    ghSetToken,
    glStatus,
    glSetToken,
    cbStatus,
    cbSetToken,
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
    providerLabel,
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

  type Tab = 'waiting' | 'repos' | 'releases';

  let viewer: Viewer | null = $state(null);
  let gl: GitLabStatus | null = $state(null);
  let cb: CodebergStatus | null = $state(null);
  let items: WaitingItem[] = $state([]);
  let repos: Repo[] = $state([]);
  let reposLoaded = $state(false);
  let reposLoading = $state(false);
  let locals: LocalRepo[] = $state([]);
  let releases: Release[] = $state([]);
  let releasesLoaded = $state(false);
  let releasesLoading = $state(false);
  let ciRuns: CiRun[] = $state([]);
  let ciByRepo = $derived(new Map(ciRuns.map((r) => [r.repo_id, r.status] as [string, CiStatus])));
  let activeTab: Tab = $state('waiting');
  let loading = $state(true);
  let refreshing = $state(false);
  let error: string | null = $state(null);

  // Onboarding / add-provider state.
  let chosenProvider: 'github' | 'gitlab' | 'codeberg' = $state('github');
  let tokenInput = $state('');
  let gitlabBaseInput = $state('https://gitlab.com');
  let codebergBaseInput = $state('https://codeberg.org');
  let connecting = $state(false);
  /** Forces the onboarding form to appear even when one provider is already
   *  connected, so the user can attach a second provider via the "+" button. */
  let addingAnotherProvider = $state(false);

  // Settings panel state — replaces the list view when the gear icon is clicked.
  let showSettings = $state(false);
  let settings: Settings = $state({
    scan_roots: [],
    scan_ignore: [],
    gitlab_base_url: null,
    codeberg_base_url: null,
    editor_command: null,
  });
  let savingSettings = $state(false);
  /** Local mirror of settings.editor_command so the input has its own
   *  uncommitted state and we only persist on blur / Enter. */
  let editorInput = $state('');
  $effect(() => {
    editorInput = settings.editor_command ?? '';
  });

  async function persistEditorCommand() {
    const next = editorInput.trim();
    const normalised = next.length === 0 ? null : next;
    if (normalised === (settings.editor_command ?? null)) return;
    settings = { ...settings, editor_command: normalised };
    await persistSettings();
  }

  // Context menu state — shared instance, opened on right-click of any
  // row. `menuItems` is recomputed per-target when the menu opens.
  let menuOpen = $state(false);
  let menuX = $state(0);
  let menuY = $state(0);
  let menuItems: MenuItem[] = $state([]);

  let connected = $derived(viewer !== null || gl !== null || cb !== null);
  /** Onboarding takes the screen unless we're explicitly in Settings —
   *  Settings is reachable even without a connected provider (so the user
   *  can configure scan roots before adding any auth). */
  let showOnboarding = $derived(!showSettings && (!connected || addingAnotherProvider));
  let canAddGithub = $derived(viewer === null);
  let canAddGitlab = $derived(gl === null);
  let canAddCodeberg = $derived(cb === null);
  let canAddAny = $derived(canAddGithub || canAddGitlab || canAddCodeberg);
  let displayName = $derived.by(() => {
    if (viewer) return viewer.name ?? viewer.login;
    if (gl) return gl.viewer.name ?? gl.viewer.login;
    if (cb) return cb.viewer.name ?? cb.viewer.login;
    return 'there';
  });

  let localByKey = $derived(indexLocalByRemote(locals));

  /** Hosts seen in local orphan clones, filtered to those that aren't
   *  already a connected provider. Drives the quick-pick chips for both
   *  GitLab and Codeberg onboarding so the user doesn't have to retype
   *  `gitlab.gwdg.de` or `codeberg.org`. */
  function hostSuggestionsFor(target: 'gitlab' | 'codeberg'): string[] {
    const out = new Set<string>();
    for (const o of locals) {
      const h = o.remote?.host;
      if (!h) continue;
      if (h === 'github.com') continue;
      if (gl && gl.base_url.includes(h)) continue;
      if (cb && cb.base_url.includes(h)) continue;
      // Cheap heuristic: hosts containing "gitlab" are GitLab-y, anything
      // else is offered for Codeberg/Gitea. We don't gatekeep too strictly
      // — the user might know better.
      const isGitlabLike = h.includes('gitlab');
      if (target === 'gitlab' && !isGitlabLike && out.size > 0) continue;
      if (target === 'codeberg' && isGitlabLike) continue;
      out.add(h);
    }
    return Array.from(out).sort();
  }

  let gitlabHostSuggestions = $derived(hostSuggestionsFor('gitlab'));
  let codebergHostSuggestions = $derived(hostSuggestionsFor('codeberg'));

  /** Local repos whose `origin` doesn't match any of the user's known remote
   *  accounts — typically scratch clones, abandoned forks, or repos hosted
   *  somewhere we don't have a provider connected yet. Surfaced separately
   *  so the user can spot disk-only state. */
  let orphans = $derived(
    locals.filter((l) => {
      if (!l.remote || !l.remote.host) return true;
      const key = `${l.remote.host}:${l.remote.owner}/${l.remote.name}`.toLowerCase();
      return !repos.some((r) => localKeyForRepo(r) === key);
    }),
  );

  let lastSyncedAt: Date | null = $state(null);

  /** Fetch waiting items (aggregated across providers) and the local clone
   *  index in parallel. Shared between the on-mount and post-connect paths. */
  async function loadInitialData() {
    const [fetchedItems, fetchedLocals] = await Promise.all([
      listWaiting(),
      listLocalRepos().catch((e) => {
        error = `Local scan failed: ${e}`;
        return [] as LocalRepo[];
      }),
    ]);
    items = fetchedItems;
    locals = fetchedLocals;
    lastSyncedAt = new Date();
  }

  onMount(async () => {
    try {
      // Always run the local scan first — its results power the orphan-host
      // suggestion that the GitLab onboarding form shows, so we need them
      // available even with no provider connected yet.
      const localPromise = listLocalRepos().catch((e) => {
        error = `Local scan failed: ${e}`;
        return [] as LocalRepo[];
      });

      // Settings load is cheap (small JSON, OS app-config dir).
      getSettings()
        .then((s) => (settings = s))
        .catch((e) => console.error('settings load:', e));

      const [ghViewer, glRes, cbRes] = await Promise.all([
        ghStatus(),
        glStatus(),
        cbStatus(),
      ]);
      viewer = ghViewer;
      gl = glRes;
      cb = cbRes;

      // Default the onboarding tab to whichever provider can still be added.
      if (!viewer && (gl || cb)) chosenProvider = 'github';
      else if (viewer && !gl && cb) chosenProvider = 'gitlab';
      else if (viewer && gl && !cb) chosenProvider = 'codeberg';

      if (viewer || gl || cb) {
        items = await listWaiting();
        lastSyncedAt = new Date();
      }
      locals = await localPromise;
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  async function connect() {
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
      addingAnotherProvider = false;
      await loadInitialData();
    } catch (e) {
      error = String(e);
    } finally {
      connecting = false;
    }
  }

  function startAddingProvider() {
    addingAnotherProvider = true;
    error = null;
    // Default to whichever single provider still can be added; otherwise
    // leave the current chosenProvider so the tab strip is visible.
    const open = [
      canAddGithub && 'github',
      canAddGitlab && 'gitlab',
      canAddCodeberg && 'codeberg',
    ].filter(Boolean) as Array<'github' | 'gitlab' | 'codeberg'>;
    if (open.length === 1) chosenProvider = open[0];
  }

  function cancelAdding() {
    addingAnotherProvider = false;
    tokenInput = '';
    error = null;
  }

  // ── Settings actions ───────────────────────────────────────────────────

  function openSettings() {
    showSettings = true;
    error = null;
  }
  function closeSettings() {
    showSettings = false;
  }

  /** Open a native folder picker, append the chosen path to scan_roots,
   *  persist, and rescan so the new path shows up immediately. */
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

  // ── Quick actions / context menu ─────────────────────────────────────

  /** Open the context menu at the cursor with items appropriate for the
   *  given row target. */
  function openRepoMenu(e: MouseEvent, r: Repo) {
    e.preventDefault();
    const local = localByKey.get(localKeyForRepo(r));
    const localPath = local?.[0]?.path;
    const hasEditor = !!(settings.editor_command && settings.editor_command.trim());

    const items: MenuItem[] = [
      { label: 'Open in browser', onclick: () => void openUrl(r.html_url) },
    ];
    if (localPath) {
      items.push({ label: 'Show in Finder', onclick: () => void revealItemInDir(localPath) });
      if (hasEditor) {
        items.push({
          label: `Open in editor (${settings.editor_command?.trim()})`,
          onclick: async () => {
            try {
              await runEditor(localPath);
            } catch (err) {
              error = String(err);
            }
          },
        });
      }
    }
    items.push({ separator: true });
    if (r.clone_url) {
      items.push({
        label: 'Copy clone URL (HTTPS)',
        onclick: () => void writeText(r.clone_url ?? ''),
      });
    }
    if (r.ssh_url) {
      items.push({
        label: 'Copy clone URL (SSH)',
        onclick: () => void writeText(r.ssh_url ?? ''),
      });
    }

    showMenu(e, items);
  }

  function openLocalRepoMenu(e: MouseEvent, l: LocalRepo) {
    e.preventDefault();
    const hasEditor = !!(settings.editor_command && settings.editor_command.trim());
    const items: MenuItem[] = [
      { label: 'Show in Finder', onclick: () => void revealItemInDir(l.path) },
    ];
    if (hasEditor) {
      items.push({
        label: `Open in editor (${settings.editor_command?.trim()})`,
        onclick: async () => {
          try {
            await runEditor(l.path);
          } catch (err) {
            error = String(err);
          }
        },
      });
    }
    if (l.remote?.raw_url) {
      items.push({ separator: true });
      items.push({
        label: 'Copy origin URL',
        onclick: () => void writeText(l.remote?.raw_url ?? ''),
      });
    }
    showMenu(e, items);
  }

  function openItemMenu(e: MouseEvent, item: WaitingItem) {
    e.preventDefault();
    showMenu(e, [
      { label: 'Open in browser', onclick: () => void openUrl(item.url) },
      { label: 'Copy URL', onclick: () => void writeText(item.url) },
    ]);
  }

  function openReleaseMenu(e: MouseEvent, r: Release) {
    e.preventDefault();
    showMenu(e, [
      { label: 'Open release', onclick: () => void openUrl(r.html_url) },
      { label: 'Copy release URL', onclick: () => void writeText(r.html_url) },
    ]);
  }

  function showMenu(e: MouseEvent, items: MenuItem[]) {
    menuItems = items;
    menuX = e.clientX;
    menuY = e.clientY;
    menuOpen = true;
  }

  async function refresh() {
    if (!connected) return;
    refreshing = true;
    error = null;
    try {
      const promises: Array<Promise<unknown>> = [
        listWaiting().then((v) => (items = v)),
        listLocalRepos().then((v) => (locals = v)),
      ];
      if (reposLoaded) {
        promises.push(listRepos().then((v) => (repos = v)));
        promises.push(listCi().then((v) => (ciRuns = v)).catch(() => {}));
      }
      if (releasesLoaded) {
        promises.push(listReleases().then((v) => (releases = v)));
      }
      await Promise.all(promises);
      lastSyncedAt = new Date();
    } catch (e) {
      error = String(e);
    } finally {
      refreshing = false;
    }
  }

  // Lazy-load repos + CI status the first time the user switches to the
  // Repos tab. With 100s of repos this can take a couple of seconds, so we
  // skip it on initial open unless the user asks.
  async function ensureRepos() {
    if (reposLoaded || reposLoading || !connected) return;
    reposLoading = true;
    try {
      const [fetchedRepos, fetchedCi] = await Promise.all([
        listRepos(),
        listCi().catch(() => [] as CiRun[]),
      ]);
      repos = fetchedRepos;
      ciRuns = fetchedCi;
      reposLoaded = true;
    } catch (e) {
      error = String(e);
    } finally {
      reposLoading = false;
    }
  }

  async function ensureReleases() {
    if (releasesLoaded || releasesLoading || !connected) return;
    releasesLoading = true;
    try {
      releases = await listReleases();
      releasesLoaded = true;
    } catch (e) {
      error = String(e);
    } finally {
      releasesLoading = false;
    }
  }

  $effect(() => {
    if (activeTab === 'repos' && connected) {
      ensureRepos();
    } else if (activeTab === 'releases' && connected) {
      ensureReleases();
    }
  });

  function repoAge(pushed_at: string | null): string {
    if (!pushed_at) return '—';
    const d = new Date(pushed_at);
    const mins = Math.floor((Date.now() - d.getTime()) / 60_000);
    if (mins < 60) return `${Math.max(1, mins)}m`;
    if (mins < 60 * 24) return `${Math.floor(mins / 60)}h`;
    if (mins < 60 * 24 * 30) return `${Math.floor(mins / (60 * 24))}d`;
    if (mins < 60 * 24 * 365) return `${Math.floor(mins / (60 * 24 * 30))}mo`;
    return `${Math.floor(mins / (60 * 24 * 365))}y`;
  }

  /** Shorten an absolute path for compact display: replace $HOME with `~`
   *  and trim to the last two path components if it's still too long. */
  function shortenPath(p: string): string {
    // The Rust side returns absolute paths; we don't know $HOME on the JS
    // side without an extra Tauri call, so we just shorten cosmetically.
    const parts = p.split('/').filter(Boolean);
    if (parts.length <= 2) return p;
    return `…/${parts.slice(-2).join('/')}`;
  }

  async function openExternal(url: string) {
    try {
      await openUrl(url);
    } catch {
      // Opener plugin failure is non-fatal — silently swallow rather than
      // poison the popover with an error toast over a missing browser handler.
    }
  }

  function humaniseSync(d: Date | null, nowMs: number): string {
    if (!d) return '—';
    const s = Math.max(0, Math.floor((nowMs - d.getTime()) / 1000));
    if (s < 5) return 'just now';
    if (s < 60) return `${s} sec ago`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m} min ago`;
    const h = Math.floor(m / 60);
    return `${h}h ago`;
  }

  // Tick once per second so the "Synced 24 sec ago" footer text counts up
  // even when no fetch is happening.
  let now = $state(Date.now());
  $effect(() => {
    const handle = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(handle);
  });
  let syncText = $derived(humaniseSync(lastSyncedAt, now));

  // Auto-refresh every 5 minutes while any provider is connected.
  const POLL_INTERVAL_MS = 5 * 60 * 1000;
  $effect(() => {
    if (!connected) return;
    const handle = setInterval(() => {
      void refresh();
    }, POLL_INTERVAL_MS);
    return () => clearInterval(handle);
  });
</script>

<div class="stage">
  <div class="pop">
    <header class="pop-head">
      <Buddy size={28} />
      <span class="brand">git<em>Buddy</em></span>
      <span class="spc"></span>
      <button
        class="ib"
        class:spin={refreshing}
        data-tip="Refresh now"
        aria-label="Refresh"
        onclick={refresh}
        disabled={!viewer || refreshing}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
          <path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 4v5h-5" />
        </svg>
      </button>
      <button class="ib" data-tip="Open main window" aria-label="Open main window">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
          <path d="M15 3h6v6" /><path d="M10 14 21 3" />
          <path d="M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5" />
        </svg>
      </button>
      <button
        class="ib"
        class:on={showSettings}
        data-tip={showSettings ? 'Back to overview' : 'Settings'}
        aria-label="Settings"
        onclick={() => (showSettings ? closeSettings() : openSettings())}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
          <circle cx="12" cy="12" r="3" />
          <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
        </svg>
      </button>
    </header>

    {#if loading}
      <div class="state-pad">
        <p class="loading-text">Connecting…</p>
      </div>
    {:else if showSettings}
      <!-- Settings panel — scan roots, connected providers, version. -->
      <div class="settings">
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

        <section class="set-sec">
          <h3>Connected <em>providers</em></h3>
          {#if !connected}
            <p class="set-empty">None yet — close Settings and pick one to start.</p>
          {:else}
            <ul class="prov-list">
              {#if viewer}
                <li class="prov-row">
                  <span class="pchip gh">gh</span>
                  <div>
                    <div class="prov-name">{viewer.name ?? viewer.login}</div>
                    <div class="prov-host">github.com</div>
                  </div>
                </li>
              {/if}
              {#if gl}
                <li class="prov-row">
                  <span class="pchip {gl.base_url.includes('gitlab.com') ? 'gl' : 'gl-self'}">
                    {gl.base_url.includes('gitlab.com') ? 'gl' : providerChipText({ provider: 'mpsd-gitlab', html_url: gl.base_url })}
                  </span>
                  <div>
                    <div class="prov-name">{gl.viewer.name ?? gl.viewer.login}</div>
                    <div class="prov-host">{new URL(gl.base_url).host}</div>
                  </div>
                </li>
              {/if}
              {#if cb}
                <li class="prov-row">
                  <span class="pchip cb">cb</span>
                  <div>
                    <div class="prov-name">{cb.viewer.name ?? cb.viewer.login}</div>
                    <div class="prov-host">{new URL(cb.base_url).host}</div>
                  </div>
                </li>
              {/if}
            </ul>
          {/if}
        </section>

        {#if error}
          <p class="err">{error}</p>
        {/if}
      </div>
    {:else if showOnboarding}
      <!-- Onboarding: no account, or user clicked "+ add provider". -->
      <div class="setup">
        {#if connected}
          <h2>Add <em>provider</em>.</h2>
          <p class="lede">
            Connect another forge to see all your work in one place.
          </p>
        {:else}
          <h2>Hi — let's <em>meet</em>.</h2>
          <p class="lede">
            Connect a Git forge to get started. Tokens are stored in your
            macOS Keychain and never sent anywhere else.
          </p>
        {/if}

        <!-- Provider selector: only render the tab strip when more than
             one slot is still empty; otherwise we'd lock to the only option. -->
        {#if [canAddGithub, canAddGitlab, canAddCodeberg].filter(Boolean).length > 1}
          <div class="provider-tabs">
            {#if canAddGithub}
              <button
                class:on={chosenProvider === 'github'}
                onclick={() => (chosenProvider = 'github')}
              >
                GitHub
              </button>
            {/if}
            {#if canAddGitlab}
              <button
                class:on={chosenProvider === 'gitlab'}
                onclick={() => (chosenProvider = 'gitlab')}
              >
                GitLab
              </button>
            {/if}
            {#if canAddCodeberg}
              <button
                class:on={chosenProvider === 'codeberg'}
                onclick={() => (chosenProvider = 'codeberg')}
              >
                Codeberg
              </button>
            {/if}
          </div>
        {/if}

        {#if chosenProvider === 'github' && canAddGithub}
          <button
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
              onkeydown={(e) => e.key === 'Enter' && connect()}
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
              onkeydown={(e) => e.key === 'Enter' && connect()}
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
              onkeydown={(e) => e.key === 'Enter' && connect()}
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
          {#if connected}
            <button type="button" class="secondary" onclick={cancelAdding} disabled={connecting}>
              Cancel
            </button>
          {/if}
          <button
            class="primary"
            onclick={connect}
            disabled={
              connecting ||
              !tokenInput.trim() ||
              (chosenProvider === 'gitlab' && !gitlabBaseInput.trim()) ||
              (chosenProvider === 'codeberg' && !codebergBaseInput.trim())
            }
          >
            {connecting ? 'Verifying…' : 'Connect'}
          </button>
        </div>
      </div>
    {:else}
      <p class="greeting">
        Hey <em>{displayName}</em> —
        {#if items.length === 0}
          you're all caught up.
        {:else}
          {items.length} {items.length === 1 ? 'thing' : 'things'} need a look.
        {/if}
        {#if canAddAny}
          <button type="button" class="add-provider" onclick={startAddingProvider}>
            + add {canAddGithub && !canAddGitlab ? 'GitHub' : canAddGitlab && !canAddGithub ? 'GitLab' : 'provider'}
          </button>
        {/if}
      </p>

      <div class="tabs" role="tablist">
        <button
          class="tab"
          class:on={activeTab === 'waiting'}
          role="tab"
          aria-selected={activeTab === 'waiting'}
          onclick={() => (activeTab = 'waiting')}
        >
          Waiting <span class="n">{items.length}</span>
        </button>
        <button
          class="tab"
          class:on={activeTab === 'repos'}
          role="tab"
          aria-selected={activeTab === 'repos'}
          onclick={() => (activeTab = 'repos')}
        >
          Repos
        </button>
        <button
          class="tab"
          class:on={activeTab === 'releases'}
          role="tab"
          aria-selected={activeTab === 'releases'}
          onclick={() => (activeTab = 'releases')}
        >
          Releases
        </button>
      </div>

      <div class="list" role="tabpanel">
        {#if error}
          <div class="err-banner">{error}</div>
        {/if}

        {#if activeTab === 'waiting'}
          {#if items.length === 0}
            <div class="empty">
              <Buddy size={48} />
              <p>Nothing's waiting on you.</p>
              <small>Last checked {syncText}.</small>
            </div>
          {:else}
            {#each items as item (item.id)}
              <button
                class="row"
                type="button"
                onclick={() => openExternal(item.url)}
                oncontextmenu={(e) => openItemMenu(e, item)}
              >
                <span class="chip {item.kind.toLowerCase()}">{item.kind}</span>
                <span class="body">
                  <span class="title">{item.title}</span>
                  <span class="meta">
                    {item.repo} <span class="dot">·</span>
                    <span class="reason">{item.reason}</span>
                    <span class="prov-tag">{providerLabel(item)}</span>
                  </span>
                </span>
                <span class="age">{item.age_human}</span>
              </button>
            {/each}
          {/if}
        {:else if activeTab === 'repos'}
          {#if reposLoading && !reposLoaded}
            <div class="empty"><p class="loading-text">Loading repos…</p></div>
          {:else if repos.length === 0}
            <div class="empty">
              <Buddy size={48} />
              <p>No repos found.</p>
              <small>Your account doesn't seem to have any visible repos.</small>
            </div>
          {:else}
            {#if reposLoaded && orphans.length > 0}
              <div class="section-h">
                Local <em>orphans</em>
                <span class="section-h-count">{orphans.length}</span>
              </div>
              {#each orphans as o (o.path)}
                <div
                  class="row repo-row orphan"
                  role="button"
                  tabindex="0"
                  oncontextmenu={(e) => openLocalRepoMenu(e, o)}
                >
                  <span class="pchip orphan-chip" data-tip="No matching remote account">?</span>
                  <span class="body">
                    <span class="title">
                      <span class="rowner">{shortenPath(o.path)}</span>
                    </span>
                    <span class="meta">
                      {#if o.branch}{o.branch}{:else if o.detached}detached{:else}—{/if}
                      {#if o.remote}<span class="dot">·</span> {o.remote.host || 'unknown host'}{/if}
                      {#if o.dirty_staged + o.dirty_unstaged > 0}
                        <span class="dot">·</span>
                        <span class="warn">{o.dirty_staged + o.dirty_unstaged} uncommitted</span>
                      {/if}
                      {#if o.ahead > 0}
                        <span class="dot">·</span>
                        <span class="warn">{o.ahead} unpushed</span>
                      {/if}
                    </span>
                  </span>
                </div>
              {/each}
              <div class="section-h">
                Remote <em>repos</em>
                <span class="section-h-count">{repos.length}</span>
              </div>
            {/if}
            {#each repos as r (r.id)}
              {@const local = localByKey.get(localKeyForRepo(r))}
              {@const localDiag = local?.[0]}
              {@const ci = ciByRepo.get(r.id) ?? 'none'}
              <button
                class="row repo-row"
                type="button"
                onclick={() => openExternal(r.html_url)}
                oncontextmenu={(e) => openRepoMenu(e, r)}
              >
                <span class="pchip {providerCssClass(r.provider)}">{providerChipText(r)}</span>
                <span class="body">
                  <span class="title">
                    {#if local}
                      <span
                        class="local-flag"
                        class:dirty={localDiag && (localDiag.dirty_staged + localDiag.dirty_unstaged + localDiag.untracked > 0 || localDiag.ahead > 0)}
                        data-tip={local.length === 1 ? `Cloned at ${localDiag?.path}` : `Cloned ${local.length}× — first at ${localDiag?.path}`}
                      ></span>
                    {/if}
                    {#if ci !== 'none'}
                      <span
                        class="ci-dot ci-{ci}"
                        data-tip={ci === 'ok' ? 'CI passing on default branch' : ci === 'fail' ? 'CI failing on default branch' : ci === 'run' ? 'CI running' : 'CI cancelled'}
                      ></span>
                    {/if}
                    <span class="rowner">{r.owner}</span><span class="rslash">/</span>{r.name}
                  </span>
                  <span class="meta">
                    {r.default_branch}
                    {#if r.language}<span class="dot">·</span> {r.language}{/if}
                    {#if r.is_private}<span class="dot">·</span> <span class="badge-private">private</span>{/if}
                    {#if r.is_fork}<span class="dot">·</span> fork{/if}
                    {#if localDiag && (localDiag.dirty_staged + localDiag.dirty_unstaged > 0)}
                      <span class="dot">·</span>
                      <span class="warn">{localDiag.dirty_staged + localDiag.dirty_unstaged} uncommitted</span>
                    {/if}
                    {#if localDiag && localDiag.ahead > 0}
                      <span class="dot">·</span>
                      <span class="warn">{localDiag.ahead} unpushed</span>
                    {/if}
                  </span>
                </span>
                <span class="age">{repoAge(r.pushed_at)}</span>
              </button>
            {/each}
          {/if}
        {:else}
          {#if releasesLoading && !releasesLoaded}
            <div class="empty"><p class="loading-text">Loading releases…</p></div>
          {:else if releases.length === 0}
            <div class="empty">
              <Buddy size={48} />
              <p>No releases found.</p>
              <small>None of your most-recent repos have published a release.</small>
            </div>
          {:else}
            {#each releases as r (r.repo_id)}
              <button
                class="row release-row"
                type="button"
                onclick={() => openExternal(r.html_url)}
                oncontextmenu={(e) => openReleaseMenu(e, r)}
              >
                <span class="pchip rel-chip">
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M12 2 4 7v10l8 5 8-5V7z" />
                    <path d="m4 7 8 5 8-5" />
                    <path d="M12 22V12" />
                  </svg>
                </span>
                <span class="body">
                  <span class="title">
                    {r.name}
                    {#if r.is_prerelease}<span class="badge-pre">pre</span>{/if}
                  </span>
                  <span class="meta">
                    {r.repo_full_name}
                    <span class="dot">·</span>
                    <span class="tag">{r.tag}</span>
                  </span>
                </span>
                <span class="age">
                  {r.age_human}
                  {#if r.is_new}<span class="new-badge">NEW</span>{/if}
                </span>
              </button>
            {/each}
          {/if}
        {/if}
      </div>
    {/if}

    <footer class="pop-foot">
      <span class="pulse" aria-hidden="true" class:idle={!viewer}></span>
      {#if viewer}
        Synced {syncText}
      {:else}
        Not connected
      {/if}
      <span class="spc"></span>
      <span class="kbd">⌘⇧G</span>
    </footer>
  </div>
</div>

<ContextMenu bind:open={menuOpen} x={menuX} y={menuY} items={menuItems} />

<style>
  /* Stage gives a transparent margin so the panel's shadow can fade naturally
     instead of being clipped to the window edge. */
  .stage {
    width: 100vw;
    height: 100vh;
    padding: 20px;
    box-sizing: border-box;
    background: transparent;
  }
  .pop {
    width: 100%;
    height: 100%;
    background: var(--paper);
    border-radius: var(--r-xl);
    box-shadow:
      0 0 0 0.5px rgba(46, 33, 27, 0.10),
      0 6px 14px -4px rgba(60, 40, 20, 0.18);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  /* Header --------------------------------------------------------- */
  .pop-head {
    padding: 16px 18px 10px;
    display: flex;
    align-items: center;
    gap: 10px;
    border-bottom: 1px solid var(--line);
    background: linear-gradient(180deg, #FFF9EC 0%, #FDF7EA 100%);
  }
  .brand {
    font-family: var(--font-display);
    font-size: 22px;
    letter-spacing: -0.02em;
    color: var(--ink);
  }
  .brand em { font-style: italic; color: var(--terracotta); }
  .spc { flex: 1; }
  .ib {
    width: 28px; height: 28px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    color: var(--ink-2);
  }
  .ib:hover:not(:disabled) { background: var(--cream-2); }
  .ib:disabled { opacity: 0.4; cursor: default; }
  .ib.spin svg {
    animation: spin 0.9s linear infinite;
  }
  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  /* Loading state -------------------------------------------------- */
  .state-pad {
    flex: 1;
    display: grid;
    place-items: center;
  }
  .loading-text {
    margin: 0;
    color: var(--ink-3);
    font-family: var(--font-display);
    font-style: italic;
    font-size: 16px;
  }

  /* Setup / onboarding -------------------------------------------- */
  .setup {
    padding: 22px 22px 24px;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 14px;
    overflow-y: auto;
  }
  .setup h2 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 400;
    font-size: 28px;
    letter-spacing: -0.02em;
    color: var(--ink);
  }
  .setup h2 em { font-style: italic; color: var(--terracotta); }
  .setup .lede {
    margin: 0;
    color: var(--ink-2);
    font-size: 13.5px;
    line-height: 1.5;
  }
  .token-link {
    align-self: flex-start;
    color: var(--terracotta);
    font-size: 13px;
    text-decoration: none;
  }
  .token-link:hover { text-decoration: underline; }
  .token-input { display: flex; flex-direction: column; gap: 6px; }
  .token-input .lbl {
    font-size: 11.5px;
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
  .primary {
    height: 38px;
    background: var(--terracotta);
    color: var(--paper);
    border-radius: var(--r-sm);
    font-weight: 600;
    font-size: 13.5px;
    transition: background 0.15s, opacity 0.15s;
  }
  .primary:hover:not(:disabled) { background: #B05738; }
  .primary:disabled { opacity: 0.5; cursor: default; }
  .secondary {
    height: 38px;
    padding: 0 14px;
    background: var(--cream-2);
    color: var(--ink-2);
    border-radius: var(--r-sm);
    font-weight: 500;
    font-size: 13px;
    transition: background 0.15s;
  }
  .secondary:hover:not(:disabled) { background: var(--cream-3); }
  .setup-actions {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .setup-actions .primary { flex: 1; }

  /* Provider segmented control — same shape as the in-list tab strip but a
     little tighter and inline in the setup form. */
  .provider-tabs {
    display: flex;
    gap: 4px;
    padding: 4px;
    background: var(--cream-2);
    border-radius: var(--r-md);
    font-size: 12.5px;
    margin-bottom: 4px;
  }
  .provider-tabs button {
    flex: 1;
    padding: 6px 8px;
    color: var(--ink-2);
    border-radius: 9px;
    text-align: center;
  }
  .provider-tabs button.on {
    background: var(--paper);
    color: var(--ink);
    font-weight: 600;
    box-shadow: var(--shadow-1);
  }

  /* Quick-pick chips for hosts seen in local orphan clones — clicking one
     fills the GitLab instance URL field so the user doesn't have to retype
     a self-hosted host name. */
  .host-hints {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: -4px;
  }
  .host-hints .hint {
    font-size: 11px;
    color: var(--ink-3);
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
  }
  .host-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .host-chip {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink);
    background: var(--paper-2);
    border: 1px solid var(--line-2);
    padding: 3px 8px;
    border-radius: 999px;
    transition: background 0.12s, border-color 0.12s;
  }
  .host-chip:hover {
    background: var(--terracotta-soft);
    border-color: var(--terracotta);
  }

  /* "+ add provider" inline link in the greeting strip — shown only when at
     least one provider slot is still empty. */
  .add-provider {
    margin-left: 6px;
    font-size: 12px;
    color: var(--terracotta);
    font-style: italic;
    font-family: var(--font-display);
  }
  .add-provider:hover { text-decoration: underline; }

  /* Settings panel ------------------------------------------------- */
  .settings {
    flex: 1;
    overflow-y: auto;
    padding: 18px 18px 22px;
    display: flex;
    flex-direction: column;
    gap: 22px;
  }
  .ib.on {
    background: var(--terracotta-soft);
    color: var(--terracotta);
  }
  .set-sec h3 {
    margin: 0 0 4px;
    font-family: var(--font-display);
    font-size: 18px;
    font-weight: 400;
    letter-spacing: -0.01em;
    color: var(--ink);
  }
  .set-sec h3 em {
    font-style: italic;
    color: var(--terracotta);
  }
  .set-help {
    margin: 0 0 12px;
    font-size: 11.5px;
    color: var(--ink-3);
    line-height: 1.5;
  }
  .set-help code {
    font-family: var(--font-mono);
    font-size: 10.5px;
    background: var(--cream-2);
    padding: 1px 4px;
    border-radius: 4px;
    color: var(--ink-2);
  }
  .set-empty {
    margin: 0 0 10px;
    font-size: 12px;
    color: var(--ink-3);
    font-style: italic;
    font-family: var(--font-display);
  }
  .path-list {
    list-style: none;
    padding: 0;
    margin: 0 0 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .path-row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 10px;
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-2);
  }
  .path-text {
    flex: 1;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .path-remove {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    color: var(--ink-3);
    font-size: 16px;
    line-height: 1;
    display: grid;
    place-items: center;
    transition: background 0.15s, color 0.15s;
  }
  .path-remove:hover:not(:disabled) {
    background: var(--terracotta-soft);
    color: var(--terracotta);
  }
  .path-remove:disabled { opacity: 0.4; cursor: default; }
  .set-add {
    align-self: flex-start;
    padding: 7px 12px;
    border: 1px dashed var(--line-2);
    border-radius: var(--r-sm);
    font-size: 12.5px;
    color: var(--ink-2);
    background: transparent;
    transition: background 0.15s, border-color 0.15s, color 0.15s;
  }
  .set-add:hover:not(:disabled) {
    background: var(--cream-2);
    border-color: var(--terracotta);
    color: var(--terracotta);
  }
  .set-add:disabled { opacity: 0.5; cursor: default; }

  .set-input {
    width: 100%;
    height: 34px;
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
  .set-input:focus {
    border-color: var(--terracotta);
    background: var(--paper);
  }

  .prov-list {
    list-style: none;
    padding: 0;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .prov-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
  }
  .prov-row .pchip {
    width: 26px; height: 26px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.04em;
    color: var(--paper);
    text-transform: lowercase;
  }
  .prov-row .pchip.gh      { background: #2E211B; }
  .prov-row .pchip.gl      { background: linear-gradient(135deg, #E89C5C, #C66243); }
  .prov-row .pchip.gl-self { background: linear-gradient(135deg, #B6A5C9, #6E5E80); }
  .prov-row .pchip.cb      { background: linear-gradient(135deg, #8DBBC9, #4E7A8A); }
  .prov-name {
    font-size: 13px;
    font-weight: 500;
    color: var(--ink);
  }
  .prov-host {
    font-size: 11px;
    color: var(--ink-3);
    font-family: var(--font-mono);
    margin-top: 1px;
  }
  .err {
    margin: 0;
    color: var(--plum);
    font-size: 12.5px;
    background: var(--plum-soft);
    padding: 8px 10px;
    border-radius: var(--r-sm);
  }
  .err-banner {
    margin: 8px 10px 0;
    color: var(--plum);
    font-size: 12px;
    background: var(--plum-soft);
    padding: 7px 10px;
    border-radius: var(--r-sm);
  }

  /* Greeting & tabs ---------------------------------------------- */
  .greeting {
    padding: 14px 18px 0;
    font-size: 13px;
    color: var(--ink-2);
    margin: 0;
  }
  .greeting em {
    font-family: var(--font-display);
    font-style: italic;
    color: var(--terracotta);
    font-weight: 400;
    font-size: 14px;
  }
  .tabs {
    margin: 12px 18px 0;
    display: flex;
    gap: 4px;
    padding: 4px;
    background: var(--cream-2);
    border-radius: var(--r-md);
    font-size: 12.5px;
  }
  .tab {
    flex: 1;
    padding: 7px 8px;
    color: var(--ink-2);
    border-radius: 9px;
    text-align: center;
  }
  .tab.on {
    background: var(--paper);
    color: var(--ink);
    font-weight: 600;
    box-shadow: var(--shadow-1);
  }
  .tab .n {
    margin-left: 5px;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-3);
  }
  .tab.on .n { color: var(--terracotta); }

  /* List ---------------------------------------------------------- */
  .list {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 8px 10px 10px;
    margin: 0;
    list-style: none;
  }
  .row {
    width: 100%;
    display: grid;
    grid-template-columns: 32px 1fr auto;
    gap: 10px;
    padding: 11px 10px;
    border-radius: var(--r-md);
    align-items: start;
    cursor: pointer;
    text-align: left;
  }
  .row:hover { background: var(--cream-2); }
  .body { display: flex; flex-direction: column; min-width: 0; }
  .title { display: block; }
  .chip {
    width: 26px; height: 26px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 600;
    margin-top: 1px;
    letter-spacing: 0.04em;
  }
  .chip.pr { background: var(--sage-soft); color: #4A6048; }
  .chip.is { background: var(--terracotta-soft); color: #A0431F; }
  .chip.mr { background: var(--butter-soft); color: #9A6E1A; }
  /* Repo rows reuse the .row layout but the leading chip is the provider
     glyph instead of an item kind. Colour distinguishes provider type at
     a glance — black for GitHub, GitLab-orange for gitlab.com, plum for
     any self-hosted GitLab, teal for Codeberg/Gitea. */
  .repo-row .pchip {
    width: 26px; height: 26px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.04em;
    margin-top: 1px;
    color: var(--paper);
    text-transform: lowercase;
  }
  .repo-row .pchip.gh      { background: #2E211B; }
  .repo-row .pchip.gl      { background: linear-gradient(135deg, #E89C5C, #C66243); }
  .repo-row .pchip.gl-self { background: linear-gradient(135deg, #B6A5C9, #6E5E80); }
  .repo-row .pchip.cb      { background: linear-gradient(135deg, #8DBBC9, #4E7A8A); }
  .repo-row .title {
    font-size: 13.5px;
    line-height: 1.3;
    color: var(--ink);
    font-weight: 500;
  }
  .repo-row .rowner { color: var(--ink-3); font-weight: 400; }
  .repo-row .rslash { color: var(--ink-4); margin: 0 1px; }
  .badge-private {
    color: var(--terracotta);
    font-style: italic;
  }
  /* Filled sage dot next to a repo name means we found a local clone of it
     in the scan roots. Goes terracotta when the local copy has uncommitted
     work or unpushed commits, so the user spots dirty clones at a glance. */
  .local-flag {
    display: inline-block;
    width: 6px;
    height: 6px;
    margin-right: 6px;
    border-radius: 50%;
    background: var(--sage);
    vertical-align: 1px;
    box-shadow: 0 0 0 2px var(--paper);
  }
  .local-flag.dirty {
    background: var(--terracotta);
  }
  /* CI status dot — a ring rather than a fill so it visually distinguishes
     itself from the solid local-clone dot above. */
  .ci-dot {
    display: inline-block;
    width: 8px;
    height: 8px;
    margin-right: 6px;
    border-radius: 50%;
    border: 1.5px solid var(--ink-4);
    vertical-align: 1px;
    background: transparent;
  }
  .ci-dot.ci-ok   { border-color: var(--sage); background: var(--sage); }
  .ci-dot.ci-fail { border-color: var(--terracotta); background: var(--terracotta); }
  .ci-dot.ci-run  {
    border-color: var(--butter);
    background: var(--butter);
    animation: ci-pulse 1.4s ease-in-out infinite;
  }
  .ci-dot.ci-cancelled { border-color: var(--ink-3); background: transparent; }
  @keyframes ci-pulse {
    0%, 100% { opacity: 1; }
    50%      { opacity: 0.45; }
  }
  .meta .warn {
    color: var(--terracotta);
    font-weight: 500;
  }

  /* Section headers inside a tab panel (e.g. "Local orphans" + "Remote repos"
     when both lists are non-empty in the Repos tab). */
  .section-h {
    padding: 14px 10px 6px;
    font-family: var(--font-display);
    font-size: 13.5px;
    color: var(--ink-2);
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .section-h:first-child { padding-top: 4px; }
  .section-h em { font-style: italic; color: var(--terracotta); }
  .section-h-count {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-3);
  }
  .repo-row.orphan {
    cursor: default;
    opacity: 0.92;
  }
  .repo-row.orphan:hover { background: transparent; }
  .orphan-chip {
    background: var(--cream-3) !important;
    color: var(--ink-3) !important;
  }

  /* Release rows reuse the .row layout. The chip uses a small package /
     octahedron icon to distinguish them from repo or waiting rows. */
  .release-row .pchip.rel-chip {
    background: var(--butter-soft);
    color: #8A5C12;
  }
  .release-row .title {
    display: flex;
    align-items: baseline;
    gap: 6px;
  }
  .release-row .tag {
    font-family: var(--font-mono);
    color: var(--ink-2);
  }
  .badge-pre {
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--plum);
    background: var(--plum-soft);
    padding: 1px 5px;
    border-radius: 999px;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    font-weight: 600;
  }
  .new-badge {
    display: inline-block;
    margin-left: 5px;
    font-family: var(--font-mono);
    font-size: 9px;
    color: var(--terracotta);
    background: var(--terracotta-soft);
    padding: 1px 5px;
    border-radius: 999px;
    letter-spacing: 0.06em;
    vertical-align: 1px;
    font-style: normal;
  }
  .title {
    font-size: 13.5px;
    line-height: 1.3;
    color: var(--ink);
    font-weight: 500;
    letter-spacing: -0.005em;
  }
  .meta {
    margin-top: 4px;
    font-size: 11px;
    color: var(--ink-3);
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
    display: flex;
    gap: 7px;
    align-items: center;
    flex-wrap: wrap;
  }
  .meta .dot { opacity: 0.5; }
  .meta .reason {
    color: var(--terracotta);
    font-weight: 500;
    text-transform: lowercase;
  }
  .meta .prov-tag {
    margin-left: auto;
    color: var(--ink-3);
    opacity: 0.7;
    font-size: 10px;
  }
  .age {
    font-family: var(--font-display);
    font-style: italic;
    font-size: 12px;
    color: var(--ink-3);
    margin-top: 2px;
  }

  .empty {
    text-align: center;
    padding: 50px 20px;
    color: var(--ink-3);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
  }
  .empty p {
    margin: 0;
    font-family: var(--font-display);
    font-size: 18px;
    color: var(--ink-2);
  }
  .empty small {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-4);
  }

  /* Footer -------------------------------------------------------- */
  .pop-foot {
    padding: 11px 16px;
    border-top: 1px solid var(--line);
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--ink-3);
    background: var(--paper-2);
    font-family: var(--font-mono);
  }
  .pulse {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sage);
    box-shadow: 0 0 0 0 rgba(128, 152, 123, 0.5);
    animation: pulse 2.4s ease-out infinite;
  }
  .pulse.idle {
    background: var(--ink-4);
    animation: none;
  }
  @keyframes pulse {
    0%   { box-shadow: 0 0 0 0 rgba(128, 152, 123, 0.5); }
    70%  { box-shadow: 0 0 0 7px rgba(128, 152, 123, 0); }
    100% { box-shadow: 0 0 0 0 rgba(128, 152, 123, 0); }
  }
  .kbd {
    background: var(--cream-2);
    padding: 2px 6px;
    border-radius: 5px;
    color: var(--ink-2);
  }
</style>
