<script lang="ts">
  import { onMount } from 'svelte';
  import { openUrl } from '@tauri-apps/plugin-opener';
  import Buddy from '$lib/Buddy.svelte';
  import {
    ghStatus,
    ghSetToken,
    ghListWaiting,
    ghListRepos,
    ghListReleases,
    listLocalRepos,
    indexLocalByRemote,
    localKeyForRepo,
    providerLabel,
    type Viewer,
    type WaitingItem,
    type Repo,
    type LocalRepo,
    type Release,
  } from '$lib/data/api';

  type Tab = 'waiting' | 'repos' | 'releases';

  let viewer: Viewer | null = $state(null);
  let items: WaitingItem[] = $state([]);
  let repos: Repo[] = $state([]);
  let reposLoaded = $state(false);
  let reposLoading = $state(false);
  let locals: LocalRepo[] = $state([]);
  let releases: Release[] = $state([]);
  let releasesLoaded = $state(false);
  let releasesLoading = $state(false);
  let activeTab: Tab = $state('waiting');
  let loading = $state(true);
  let refreshing = $state(false);
  let error: string | null = $state(null);

  let localByKey = $derived(indexLocalByRemote(locals));

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

  // Setup-form state (visible only when there's no connected account).
  let tokenInput = $state('');
  let connecting = $state(false);

  let lastSyncedAt: Date | null = $state(null);

  /** Fetch the data that should be visible the moment the user has a connected
   *  account — waiting items + local clone index, in parallel. Shared between
   *  the on-mount path and the post-connect path so the popover and the
   *  Repos-tab local indicators show up immediately in both cases (previously
   *  the post-connect path only fetched waiting items, which is why local dots
   *  only appeared after a manual refresh). */
  async function loadInitialData() {
    const [fetchedItems, fetchedLocals] = await Promise.all([
      ghListWaiting(),
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
      viewer = await ghStatus();
      if (viewer) await loadInitialData();
    } catch (e) {
      error = String(e);
    } finally {
      loading = false;
    }
  });

  async function connect() {
    if (!tokenInput.trim()) return;
    connecting = true;
    error = null;
    try {
      viewer = await ghSetToken(tokenInput.trim());
      tokenInput = '';
      await loadInitialData();
    } catch (e) {
      error = String(e);
    } finally {
      connecting = false;
    }
  }

  async function refresh() {
    if (!viewer) return;
    refreshing = true;
    error = null;
    try {
      const promises: Array<Promise<unknown>> = [
        ghListWaiting().then((v) => (items = v)),
        listLocalRepos().then((v) => (locals = v)),
      ];
      if (reposLoaded) {
        promises.push(ghListRepos().then((v) => (repos = v)));
      }
      if (releasesLoaded) {
        promises.push(ghListReleases().then((v) => (releases = v)));
      }
      await Promise.all(promises);
      lastSyncedAt = new Date();
    } catch (e) {
      error = String(e);
    } finally {
      refreshing = false;
    }
  }

  // Lazy-load repos the first time the user switches to that tab. With 100s
  // of repos this can take a couple seconds, so we skip it on initial open
  // unless the user actually asked.
  async function ensureRepos() {
    if (reposLoaded || reposLoading || !viewer) return;
    reposLoading = true;
    try {
      repos = await ghListRepos();
      reposLoaded = true;
    } catch (e) {
      error = String(e);
    } finally {
      reposLoading = false;
    }
  }

  // Releases are even more expensive (one /releases/latest per repo, capped
  // to 60 in the backend), so we also defer them until the tab is opened.
  async function ensureReleases() {
    if (releasesLoaded || releasesLoading || !viewer) return;
    releasesLoading = true;
    try {
      releases = await ghListReleases();
      releasesLoaded = true;
    } catch (e) {
      error = String(e);
    } finally {
      releasesLoading = false;
    }
  }

  $effect(() => {
    if (activeTab === 'repos' && viewer) {
      ensureRepos();
    } else if (activeTab === 'releases' && viewer) {
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

  function providerInitial(p: Repo): string {
    return ({
      github: 'gh',
      gitlab: 'gl',
      codeberg: 'cb',
      'mpsd-gitlab': 'mp',
    } as const)[p.provider];
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
        title="Refresh"
        aria-label="Refresh"
        onclick={refresh}
        disabled={!viewer || refreshing}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
          <path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 4v5h-5" />
        </svg>
      </button>
      <button class="ib" title="Open main window" aria-label="Open main window">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
          <path d="M15 3h6v6" /><path d="M10 14 21 3" />
          <path d="M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5" />
        </svg>
      </button>
      <button class="ib" title="Settings" aria-label="Settings">
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
    {:else if !viewer}
      <!-- Onboarding: no account configured yet. -->
      <div class="setup">
        <h2>Hi — let's <em>meet</em>.</h2>
        <p class="lede">
          Paste a GitHub personal access token to start. gitBuddy stores it in
          your macOS Keychain and never sends it anywhere else.
        </p>

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

        {#if error}
          <p class="err">{error}</p>
        {/if}

        <button
          class="primary"
          onclick={connect}
          disabled={connecting || !tokenInput.trim()}
        >
          {connecting ? 'Verifying…' : 'Connect'}
        </button>
      </div>
    {:else}
      <p class="greeting">
        Hey <em>{viewer.name ?? viewer.login}</em> —
        {#if items.length === 0}
          you're all caught up.
        {:else}
          {items.length} {items.length === 1 ? 'thing' : 'things'} need a look.
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
              <button class="row" type="button" onclick={() => openExternal(item.url)}>
                <span class="chip {item.kind.toLowerCase()}">{item.kind}</span>
                <span class="body">
                  <span class="title">{item.title}</span>
                  <span class="meta">
                    {item.repo} <span class="dot">·</span>
                    <span class="reason">{item.reason}</span>
                    <span class="prov-tag">{providerLabel[item.provider]}</span>
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
                <div class="row repo-row orphan">
                  <span class="pchip orphan-chip" title="No matching remote account">?</span>
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
              <button class="row repo-row" type="button" onclick={() => openExternal(r.html_url)}>
                <span class="pchip">{providerInitial(r)}</span>
                <span class="body">
                  <span class="title">
                    {#if local}
                      <span
                        class="local-flag"
                        class:dirty={localDiag && (localDiag.dirty_staged + localDiag.dirty_unstaged + localDiag.untracked > 0 || localDiag.ahead > 0)}
                        title={local.length === 1 ? `Cloned at ${localDiag?.path}` : `Cloned ${local.length}× — first at ${localDiag?.path}`}
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
              <button class="row release-row" type="button" onclick={() => openExternal(r.html_url)}>
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
     glyph instead of an item kind. */
  .repo-row .pchip {
    width: 26px; height: 26px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 600;
    letter-spacing: 0.04em;
    margin-top: 1px;
    background: #2E211B;
    color: var(--paper);
    text-transform: lowercase;
  }
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
