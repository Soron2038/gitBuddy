<script lang="ts">
  import { onMount } from 'svelte';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { writeText } from '@tauri-apps/plugin-clipboard-manager';
  import { listen } from '@tauri-apps/api/event';
  import Buddy from '$lib/Buddy.svelte';
  import ContextMenu, { type MenuItem } from '$lib/ContextMenu.svelte';
  import {
    ghSetToken,
    ghOAuthBegin,
    ghOAuthPoll,
    glSetToken,
    cbSetToken,
    accountsList,
    accountsDisconnect,
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
    providerLabel,
    type Provider,
    type Viewer,
    type GitLabStatus,
    type CodebergStatus,
    type Account,
    type WaitingItem,
    type ItemReason,
    type Repo,
    type LocalRepo,
    type Release,
    type CiRun,
    type CiStatus,
    type Settings,
  } from '$lib/data/api';

  type View = 'overview' | 'settings';
  type Status = 'on-you' | 'all' | 'releases' | 'local';

  // ── Auth state ────────────────────────────────────────────────────────
  // `accounts` is the source of truth. The viewer/gl/cb fields are kept as
  // raw state (rather than $derived) so the rest of the legacy UI that
  // assumes a single account per provider type keeps working unchanged —
  // every refreshAuth() rebuilds them by picking the first account of each
  // type out of `accounts`.
  let accounts: Account[] = $state([]);
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

  // ── GitHub OAuth Device Flow state ─────────────────────────────────────
  // The frontend drives both the polling cadence and the countdown timer;
  // the backend stays stateless beyond holding the eventual access token.
  type GithubAuthMethod = 'oauth' | 'pat';
  type OAuthState = 'idle' | 'awaiting' | 'error';
  let githubAuthMethod: GithubAuthMethod = $state('oauth');
  let oauthState: OAuthState = $state('idle');
  let oauthUserCode = $state('');
  let oauthDeviceCode = $state('');
  let oauthVerificationUri = $state('');
  let oauthExpiresIn = $state(0);
  let oauthInterval = $state(5);
  let oauthRemaining = $state(0);
  let oauthErrorMsg = $state('');
  let oauthCopied = $state(false);
  let oauthPollHandle: ReturnType<typeof setTimeout> | null = null;
  let oauthCountdownHandle: ReturnType<typeof setInterval> | null = null;

  // Context menu (right-click on repo cards).
  let menuOpen = $state(false);
  let menuX = $state(0);
  let menuY = $state(0);
  let menuItems: MenuItem[] = $state([]);

  // ── Detail pane ──────────────────────────────────────────────────────
  let selectedRepo = $state<Repo | null>(null);

  // ── Filters / Search ─────────────────────────────────────────────────
  let status = $state<Status>('all');
  let searchQuery = $state('');
  let reasonFilter = $state<Set<ItemReason>>(
    new Set<ItemReason>(['assigned', 'review', 'authored', 'mentioned']),
  );
  /** Account ids the user wants to see. The set starts containing every
   *  connected account; toggling a chip removes/adds. An effect reconciles
   *  the set whenever the accounts list changes: newly added accounts
   *  auto-appear in views (added to the set), disconnected ones drop out
   *  (removed from the set). Same Set-membership shape as `reasonFilter`. */
  let accountFilter = $state<Set<string>>(new Set());
  let searchInputEl = $state<HTMLInputElement | null>(null);

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
  /** O(1) lookup of an account by id — driven by every UI surface that
   *  renders per-account chips/badges. Recomputed only when `accounts`
   *  changes. */
  let accountById = $derived(new Map(accounts.map((a) => [a.id, a])));
  /** Distinct repos in the (unfiltered) raw list — used for the
   *  "{shown} of {total}" label so the denominator reflects deduped
   *  repos rather than the post-fan-out row count. */
  let repoTotalCount = $derived(new Set(repos.map((r) => r.html_url)).size);
  /** When true, the repo card shows one chip per contributing account so
   *  the user can tell which of their accounts surfaced each row. With
   *  only one account, those chips would be redundant noise. */
  let showAccountBadges = $derived(accounts.length > 1);

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
  let providerCount = $derived(accounts.length);

  // Multi-account: every provider tab is always available — you can stack
  // multiple GitLab instances, a personal + work GitHub, etc. The constant
  // list keeps the tab order stable across renders.
  const availableProviderTabs = ['github', 'gitlab', 'codeberg'] as const;

  /** Hosts seen in local orphan clones, filtered by which provider tab the
   *  user is in. Drives the quick-pick chips for GitLab and Codeberg
   *  onboarding so self-hosted hostnames don't have to be retyped. Hosts
   *  already connected via any account are filtered out to avoid offering
   *  duplicates. */
  function hostSuggestionsFor(target: 'gitlab' | 'codeberg'): string[] {
    const alreadyConnected = new Set<string>();
    for (const a of accounts) {
      if (!a.base_url) continue;
      try {
        alreadyConnected.add(new URL(a.base_url).host);
      } catch {
        /* malformed base_url — skip */
      }
    }
    const out = new Set<string>();
    for (const o of locals) {
      const h = o.remote?.host;
      if (!h) continue;
      if (h === 'github.com') continue;
      if (alreadyConnected.has(h)) continue;
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
    /** Account.id — used as Svelte each-key and as the click-through target. */
    accountId: string;
    kind: 'github' | 'gitlab' | 'codeberg';
    viewer: Viewer;
    host: string;
  };
  let connectedProviders = $derived.by(() => {
    const out: ProvBadge[] = [];
    for (const a of accounts) {
      const host = a.base_url
        ? (() => {
            try {
              return new URL(a.base_url!).host;
            } catch {
              return '';
            }
          })()
        : 'github.com';
      if (!host) continue;
      const kind =
        a.provider === 'github'
          ? 'github'
          : a.provider === 'codeberg'
            ? 'codeberg'
            : 'gitlab';
      out.push({ accountId: a.id, kind, viewer: a.viewer, host });
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
    // Count of distinct repos this specific account surfaces. Two accounts
    // on the same host (and the same repo seen via both) each get their own
    // count, which matches the per-account chip's mental model.
    const seen = new Set<string>();
    for (const r of repos) {
      if (r.account_id !== p.accountId) continue;
      seen.add(r.html_url);
    }
    return seen.size;
  }

  // ── Filter helpers ───────────────────────────────────────────────────

  /** True iff the item's source account is selected in the filter. Records
   *  without an account_id (defensive — aggregator always sets it) pass
   *  through, since dropping unattributed data is worse than showing it.
   *  Supersedes the older host-based `disabledHosts` filter (M6.4): one
   *  account-id is more granular than a host since two accounts can
   *  share a host. */
  function isAccountSelected(item: { account_id: string | null }): boolean {
    if (!item.account_id) return true;
    return accountFilter.has(item.account_id);
  }

  // Keep accountFilter in lockstep with the accounts list: newly connected
  // accounts auto-appear in views; disconnected ones drop out.
  $effect(() => {
    const liveIds = new Set(accounts.map((a) => a.id));
    let changed = false;
    const next = new Set(accountFilter);
    for (const id of liveIds) {
      if (!next.has(id)) {
        next.add(id);
        changed = true;
      }
    }
    for (const id of accountFilter) {
      if (!liveIds.has(id)) {
        next.delete(id);
        changed = true;
      }
    }
    if (changed) accountFilter = next;
  });

  function toggleAccountFilter(id: string) {
    const next = new Set(accountFilter);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    accountFilter = next;
  }
  function selectOnlyAccount(id: string) {
    accountFilter = new Set([id]);
  }
  function selectAllAccounts() {
    accountFilter = new Set(accounts.map((a) => a.id));
  }

  function matchesSearch(r: Repo, q: string): boolean {
    if (!q) return true;
    const hay = `${r.owner}/${r.name} ${r.description ?? ''}`.toLowerCase();
    return hay.includes(q);
  }
  function matchesSearchItem(it: WaitingItem, q: string): boolean {
    if (!q) return true;
    return `${it.repo} ${it.title}`.toLowerCase().includes(q);
  }
  function matchesSearchRelease(rel: Release, q: string): boolean {
    if (!q) return true;
    return `${rel.repo_full_name} ${rel.name} ${rel.tag}`.toLowerCase().includes(q);
  }

  let normalisedQuery = $derived(searchQuery.trim().toLowerCase());

  let searchPlaceholder = $derived(
    status === 'on-you'
      ? 'Filter waiting items…'
      : status === 'releases'
        ? 'Filter releases…'
        : status === 'local'
          ? 'Filter local clones…'
          : 'Search repos by name, owner, description…',
  );

  /** A Repo enriched with the list of accounts that surfaced it — after
   *  dedup, one entry per unique html_url with a badge for each origin
   *  account. The aggregator in `list_repos` returns one row per
   *  (account, repo) pair; this is where we collapse them for display. */
  type RepoEntry = Repo & { account_ids: string[] };

  let filteredRepos = $derived.by((): RepoEntry[] => {
    const filtered = repos.filter(
      (r) => isAccountSelected(r) && matchesSearch(r, normalisedQuery),
    );
    const map = new Map<string, RepoEntry>();
    for (const r of filtered) {
      const existing = map.get(r.html_url);
      if (existing) {
        if (r.account_id && !existing.account_ids.includes(r.account_id)) {
          existing.account_ids.push(r.account_id);
        }
      } else {
        map.set(r.html_url, {
          ...r,
          account_ids: r.account_id ? [r.account_id] : [],
        });
      }
    }
    return Array.from(map.values());
  });
  let filteredLocals = $derived(
    filteredRepos.filter((r) => localByKey.has(localKeyForRepo(r))),
  );
  let filteredItems = $derived(
    items.filter(
      (it) =>
        isAccountSelected(it) &&
        reasonFilter.has(it.reason) &&
        matchesSearchItem(it, normalisedQuery),
    ),
  );
  let filteredReleases = $derived(
    releases.filter(
      (rel) =>
        rel.is_new &&
        isAccountSelected(rel) &&
        matchesSearchRelease(rel, normalisedQuery),
    ),
  );

  function toggleReason(reason: ItemReason) {
    const next = new Set(reasonFilter);
    if (next.has(reason)) next.delete(reason);
    else next.add(reason);
    reasonFilter = next;
  }

  function providerHomeUrl(p: ProvBadge): string {
    return `https://${p.host}/${p.viewer.login}`;
  }

  // ── Detail-pane derivations ─────────────────────────────────────────
  // Modus-Wechsel verwirft die Selektion: das selektierte Repo könnte im
  // neuen Filter gar nicht mehr existieren.
  $effect(() => {
    void status;
    selectedRepo = null;
  });

  function repoFullName(r: Repo): string {
    return `${r.owner}/${r.name}`;
  }

  let detailPaneOpen = $derived(
    selectedRepo !== null && (status === 'all' || status === 'local'),
  );

  let selectedFullName = $derived(
    selectedRepo ? repoFullName(selectedRepo) : null,
  );
  let selectedLocalDiag = $derived(
    selectedRepo
      ? (localByKey.get(localKeyForRepo(selectedRepo)) ?? [])
      : [],
  );
  let selectedCi = $derived(
    selectedRepo ? (ciRuns.find((c) => c.repo_id === selectedRepo!.id) ?? null) : null,
  );
  let selectedRelease = $derived.by(() => {
    if (!selectedFullName) return null;
    return (
      releases.find((r) => r.repo_full_name === selectedFullName) ?? null
    );
  });
  let selectedItems = $derived(
    selectedFullName
      ? items.filter((it) => it.repo === selectedFullName)
      : [],
  );
  let selectedEditorCmd = $derived(settings.editor_command?.trim() ?? '');

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
    accounts = await accountsList();
    // Legacy single-account-per-provider helpers used by the rest of the
    // UI; rebuilt from the canonical accounts list each refresh. Picking
    // "first match per provider" preserves the existing render paths
    // while the multi-account-aware Account-Liste in Settings shows
    // every entry.
    const gh = accounts.find((a) => a.provider === 'github');
    const glAcct = accounts.find((a) => a.provider === 'gitlab' || a.provider === 'mpsd-gitlab');
    const cbAcct = accounts.find((a) => a.provider === 'codeberg');
    viewer = gh?.viewer ?? null;
    gl = glAcct && glAcct.base_url
      ? { viewer: glAcct.viewer, base_url: glAcct.base_url }
      : null;
    cb = cbAcct && cbAcct.base_url
      ? { viewer: cbAcct.viewer, base_url: cbAcct.base_url }
      : null;
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

  // ── Window-local keyboard shortcuts ─────────────────────────────────
  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (view !== 'overview') return;
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault();
        searchInputEl?.focus();
        searchInputEl?.select();
        return;
      }
      if (e.key === 'Escape' && selectedRepo !== null) {
        // Wenn ein Input fokussiert ist, lass Escape den Fokus räumen,
        // statt direkt die Selektion zu schließen.
        const target = e.target as HTMLElement | null;
        const inField =
          target && (target.tagName === 'INPUT' || target.tagName === 'TEXTAREA');
        if (inField) return;
        selectedRepo = null;
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
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

  function showMenu(e: MouseEvent, m: MenuItem[]) {
    e.preventDefault();
    menuItems = m;
    menuX = e.clientX;
    menuY = e.clientY;
    menuOpen = true;
  }

  function openItemMenu(e: MouseEvent, it: WaitingItem) {
    showMenu(e, [
      { label: 'Open in browser', onclick: () => openExternal(it.url) },
      { label: 'Copy URL', onclick: () => writeText(it.url) },
    ]);
  }

  function openReleaseMenu(e: MouseEvent, rel: Release) {
    showMenu(e, [
      { label: 'Open release', onclick: () => openExternal(rel.html_url) },
      { label: 'Copy release URL', onclick: () => writeText(rel.html_url) },
    ]);
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

  async function disconnectAccount(account: Account) {
    const where = account.base_url
      ? (() => {
          try {
            return new URL(account.base_url!).host;
          } catch {
            return account.base_url!;
          }
        })()
      : 'github.com';
    if (
      !confirm(
        `Disconnect ${account.login} (${where})? The stored token will be removed from your Keychain.`,
      )
    ) {
      return;
    }
    error = null;
    try {
      await accountsDisconnect(account.id);
      // Optimistic local update so the row disappears without waiting for
      // the provider-changed event to bounce back via Tauri.
      accounts = accounts.filter((a) => a.id !== account.id);
      await refreshAuth();
    } catch (e) {
      error = String(e);
    }
  }

  function startAddingProvider() {
    addingProvider = true;
    tokenInput = '';
    error = null;
    githubAuthMethod = 'oauth';
    resetOAuthState();
    // Multi-account: every provider tab is always available. Default to
    // GitHub if the previous chosen tab is no longer in the (constant)
    // list — kept defensive in case the tab list shrinks again later.
    if (!availableProviderTabs.includes(chosenProvider)) {
      chosenProvider = 'github';
    }
  }

  function cancelAddingProvider() {
    addingProvider = false;
    tokenInput = '';
    error = null;
    resetOAuthState();
  }

  // ── GitHub OAuth Device Flow ─────────────────────────────────────────

  function resetOAuthState() {
    if (oauthPollHandle) {
      clearTimeout(oauthPollHandle);
      oauthPollHandle = null;
    }
    if (oauthCountdownHandle) {
      clearInterval(oauthCountdownHandle);
      oauthCountdownHandle = null;
    }
    oauthState = 'idle';
    oauthUserCode = '';
    oauthDeviceCode = '';
    oauthVerificationUri = '';
    oauthExpiresIn = 0;
    oauthInterval = 5;
    oauthRemaining = 0;
    oauthErrorMsg = '';
    oauthCopied = false;
  }

  async function startGithubOAuth() {
    resetOAuthState();
    connecting = true;
    error = null;
    try {
      const res = await ghOAuthBegin();
      oauthUserCode = res.user_code;
      oauthDeviceCode = res.device_code;
      oauthVerificationUri = res.verification_uri;
      oauthExpiresIn = res.expires_in;
      oauthRemaining = res.expires_in;
      oauthInterval = Math.max(res.interval, 1);
      oauthState = 'awaiting';

      // Auto-open the verification URL — same UX as the existing
      // "Create a token →" links.
      openExternal(oauthVerificationUri).catch(() => {
        // If the browser refuses to open, the secondary link in the UI
        // still works as a fallback; don't surface as an error.
      });

      startOAuthCountdown();
      scheduleOAuthPoll(oauthInterval);
    } catch (e) {
      oauthErrorMsg = String(e);
      oauthState = 'error';
    } finally {
      connecting = false;
    }
  }

  function startOAuthCountdown() {
    if (oauthCountdownHandle) clearInterval(oauthCountdownHandle);
    oauthCountdownHandle = setInterval(() => {
      oauthRemaining = Math.max(0, oauthRemaining - 1);
      if (oauthRemaining === 0 && oauthCountdownHandle) {
        clearInterval(oauthCountdownHandle);
        oauthCountdownHandle = null;
      }
    }, 1000);
  }

  function scheduleOAuthPoll(seconds: number) {
    if (oauthPollHandle) clearTimeout(oauthPollHandle);
    oauthPollHandle = setTimeout(() => {
      void runOAuthPoll();
    }, seconds * 1000);
  }

  async function runOAuthPoll() {
    if (oauthState !== 'awaiting' || !oauthDeviceCode) return;
    try {
      const r = await ghOAuthPoll(oauthDeviceCode);
      switch (r.kind) {
        case 'pending':
          scheduleOAuthPoll(oauthInterval);
          break;
        case 'slow_down':
          oauthInterval = Math.max(r.interval, oauthInterval + 1);
          scheduleOAuthPoll(oauthInterval);
          break;
        case 'denied':
          oauthErrorMsg = 'GitHub sign-in was denied. Try again to grant access.';
          oauthState = 'error';
          if (oauthCountdownHandle) {
            clearInterval(oauthCountdownHandle);
            oauthCountdownHandle = null;
          }
          break;
        case 'expired':
          oauthErrorMsg = 'The code expired before approval. Start over to get a fresh one.';
          oauthState = 'error';
          if (oauthCountdownHandle) {
            clearInterval(oauthCountdownHandle);
            oauthCountdownHandle = null;
          }
          break;
        case 'success':
          resetOAuthState();
          addingProvider = false;
          await refreshAuth();
          await loadAllData();
          break;
      }
    } catch (e) {
      // Network/parse failure: keep the flow alive — the next scheduled
      // poll might succeed. Show a soft inline error so the user knows
      // why progress stalled.
      oauthErrorMsg = String(e);
      oauthState = 'error';
    }
  }

  async function copyUserCode() {
    if (!oauthUserCode) return;
    try {
      await writeText(oauthUserCode);
      oauthCopied = true;
      setTimeout(() => (oauthCopied = false), 1600);
    } catch {
      // Ignore — the code is still visible in the UI.
    }
  }

  async function connectProvider() {
    if (!tokenInput.trim()) return;
    if (chosenProvider === 'gitlab' && !gitlabBaseInput.trim()) return;
    if (chosenProvider === 'codeberg' && !codebergBaseInput.trim()) return;
    connecting = true;
    error = null;
    try {
      if (chosenProvider === 'github') {
        await ghSetToken(tokenInput.trim());
      } else if (chosenProvider === 'gitlab') {
        await glSetToken(tokenInput.trim(), gitlabBaseInput.trim());
      } else {
        await cbSetToken(tokenInput.trim(), codebergBaseInput.trim());
      }
      tokenInput = '';
      addingProvider = false;
      await refreshAuth();
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
        <input
          type="text"
          placeholder={searchPlaceholder}
          bind:value={searchQuery}
          bind:this={searchInputEl}
          disabled={!connected}
          spellcheck="false"
          autocomplete="off"
        />
        {#if searchQuery}
          <button
            type="button"
            class="search-clear"
            onclick={() => (searchQuery = '')}
            aria-label="Clear search"
          >
            ×
          </button>
        {/if}
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

    {#if status === 'on-you' && connected}
      <div class="reason-chips">
        <span class="reason-chips-label">Reasons</span>
        {#each ['assigned', 'review', 'authored', 'mentioned'] as r}
          {@const reason = r as ItemReason}
          {@const on = reasonFilter.has(reason)}
          <button
            type="button"
            class="chip-toggle"
            class:on
            onclick={() => toggleReason(reason)}
            aria-pressed={on}
          >
            {reason === 'review' ? 'review requested' : reason}
          </button>
        {/each}
      </div>
    {/if}

    <div class="body" class:has-detail={detailPaneOpen}>
      <aside class="side">
        <section class="sec">
          <h3>What's <em>waiting</em></h3>
          <button
            type="button"
            class="pill pill-btn"
            class:on={status === 'on-you'}
            onclick={() => (status = 'on-you')}
          >
            <span class="sw t"></span> On you <span class="c">{waitingCount}</span>
          </button>
          <button
            type="button"
            class="pill pill-btn"
            class:on={status === 'all'}
            onclick={() => (status = 'all')}
          >
            <span class="sw s"></span> All repos <span class="c">{repoTotalCount}</span>
          </button>
          <button
            type="button"
            class="pill pill-btn"
            class:on={status === 'releases'}
            onclick={() => (status = 'releases')}
          >
            <span class="sw b"></span> New releases <span class="c">{newReleasesCount}</span>
          </button>
          <button
            type="button"
            class="pill pill-btn"
            class:on={status === 'local'}
            onclick={() => (status = 'local')}
          >
            <span class="sw p"></span> Local clones <span class="c">{localCount}</span>
          </button>
        </section>

        <section class="sec">
          <h3>Accounts</h3>
          {#if connectedProviders.length === 0}
            <p class="side-empty">No providers connected yet.</p>
          {:else}
            {#each connectedProviders as p (p.accountId)}
              {@const on = accountFilter.has(p.accountId)}
              <div class="pill acct-pill" class:muted={!on}>
                <button
                  type="button"
                  class="acct-toggle"
                  onclick={(e) => {
                    // Plain click toggles inclusion; ⌥/Alt-click isolates to
                    // just this account (and ⌥-clicking the only-on chip
                    // re-selects all). Mirrors macOS list multi-select.
                    if (e.altKey) {
                      if (accountFilter.size === 1 && on) selectAllAccounts();
                      else selectOnlyAccount(p.accountId);
                    } else {
                      toggleAccountFilter(p.accountId);
                    }
                  }}
                  aria-pressed={on}
                  data-tip={on
                    ? 'Hide this account · ⌥-click to solo'
                    : 'Show this account · ⌥-click to solo'}
                >
                  <span class="ava {avatarClass(p)}">{avatarText(p)}</span>
                  <span class="acct-name">
                    {p.viewer.login}
                    <span class="acct-host">{p.host}</span>
                  </span>
                  <span class="c">{repoCountForProvider(p)}</span>
                </button>
                <button
                  type="button"
                  class="acct-open"
                  onclick={() => openExternal(providerHomeUrl(p))}
                  data-tip="Open profile in browser"
                  aria-label="Open {p.viewer.login} on {p.host}"
                >
                  <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                    <path d="M7 17 17 7" />
                    <path d="M7 7h10v10" />
                  </svg>
                </button>
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

          {#snippet repoCardEntry(r: RepoEntry)}
            {@const local = localByKey.get(localKeyForRepo(r))}
            {@const localDiag = local?.[0]}
            {@const ci = ciByRepo.get(r.id) ?? 'none'}
            <button
              class="card"
              class:selected={selectedRepo?.id === r.id}
              onclick={() => (selectedRepo = selectedRepo?.id === r.id ? null : r)}
              oncontextmenu={(e) => openRepoMenu(e, r)}
            >
              {#if showAccountBadges && r.account_ids.length > 0}
                <span class="acct-badges">
                  {#each r.account_ids as id (id)}
                    {@const a = accountById.get(id)}
                    {#if a}
                      {@const aHost = a.base_url
                        ? (() => {
                            try {
                              return new URL(a.base_url!).host;
                            } catch {
                              return a.base_url!;
                            }
                          })()
                        : 'github.com'}
                      <span
                        class="pchip {providerCssClass(a.provider)}"
                        title="{a.login}@{aHost}"
                      >
                        {providerChipText({ provider: a.provider, html_url: a.base_url ?? '' })}
                      </span>
                    {/if}
                  {/each}
                </span>
              {:else}
                <span class="pchip {providerCssClass(r.provider)}">{providerChipText(r)}</span>
              {/if}
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
          {/snippet}

          {#if status === 'all'}
            <h2 class="section-h">
              Your <em>repos</em>
              <span class="count">
                {filteredRepos.length} shown{#if filteredRepos.length !== repoTotalCount}
                  <span class="muted-count"> · of {repoTotalCount}</span>
                {/if}
              </span>
            </h2>

            {#if repos.length === 0}
              <p class="content-empty">
                None of your accounts surfaced any repos yet. If you just connected,
                give the first sync a moment — or hit Refresh.
              </p>
            {:else if filteredRepos.length === 0}
              <p class="content-empty">
                No repos match these filters.
              </p>
            {:else}
              <div class="repo-grid">
                {#each filteredRepos as r (r.id)}
                  {@render repoCardEntry(r)}
                {/each}
              </div>
            {/if}
          {:else if status === 'on-you'}
            <h2 class="section-h">
              Waiting on <em>you</em>
              <span class="count">
                {filteredItems.length} shown{#if filteredItems.length !== items.length}
                  <span class="muted-count"> · of {items.length}</span>
                {/if}
              </span>
            </h2>

            {#if items.length === 0}
              <p class="content-empty">
                You're all caught up — nothing waiting on you right now.
              </p>
            {:else if filteredItems.length === 0}
              <p class="content-empty">
                No items match these filters.
              </p>
            {:else}
              <div class="row-list">
                {#each filteredItems as it (it.id)}
                  <button
                    type="button"
                    class="row"
                    onclick={() => openExternal(it.url)}
                    oncontextmenu={(e) => openItemMenu(e, it)}
                  >
                    <span class="kind-chip {it.kind.toLowerCase()}">{it.kind}</span>
                    <span class="row-body">
                      <span class="row-title">{it.title}</span>
                      <span class="row-meta">
                        <span class="row-repo">{it.repo}</span>
                        <span class="row-dot">·</span>
                        <span class="row-reason">{it.reason === 'review' ? 'review requested' : it.reason}</span>
                        <span class="row-dot">·</span>
                        <span class="row-prov">{providerLabel(it)}</span>
                      </span>
                    </span>
                    <span class="row-age">{it.age_human}</span>
                  </button>
                {/each}
              </div>
            {/if}
          {:else if status === 'releases'}
            <h2 class="section-h">
              New <em>releases</em>
              <span class="count">
                {filteredReleases.length} shown{#if filteredReleases.length !== newReleasesCount}
                  <span class="muted-count"> · of {newReleasesCount}</span>
                {/if}
              </span>
            </h2>

            {#if newReleasesCount === 0}
              <p class="content-empty">
                No fresh releases in the last week.
              </p>
            {:else if filteredReleases.length === 0}
              <p class="content-empty">
                No releases match these filters.
              </p>
            {:else}
              <div class="row-list">
                {#each filteredReleases as rel (rel.repo_id + ':' + rel.tag)}
                  <button
                    type="button"
                    class="row release-row"
                    onclick={() => openExternal(rel.html_url)}
                    oncontextmenu={(e) => openReleaseMenu(e, rel)}
                  >
                    <span class="kind-chip rel">
                      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
                        <path d="M12 2 4 7v10l8 5 8-5V7z" />
                        <path d="m4 7 8 5 8-5" />
                        <path d="M12 22V12" />
                      </svg>
                    </span>
                    <span class="row-body">
                      <span class="row-title">
                        {rel.name || rel.tag}
                        {#if rel.is_prerelease}<span class="badge-pre">pre</span>{/if}
                      </span>
                      <span class="row-meta">
                        <span class="row-repo">{rel.repo_full_name}</span>
                        <span class="row-dot">·</span>
                        <span class="row-tag">{rel.tag}</span>
                      </span>
                    </span>
                    <span class="row-age">
                      {rel.age_human}
                      {#if rel.is_new}<span class="new-badge">NEW</span>{/if}
                    </span>
                  </button>
                {/each}
              </div>
            {/if}
          {:else}
            <h2 class="section-h">
              Local <em>clones</em>
              <span class="count">
                {filteredLocals.length} shown{#if filteredLocals.length !== localCount}
                  <span class="muted-count"> · of {localCount}</span>
                {/if}
              </span>
            </h2>

            {#if localCount === 0}
              <p class="content-empty">
                No local clones found in your scan roots. Add a folder in Settings.
              </p>
            {:else if filteredLocals.length === 0}
              <p class="content-empty">
                No local clones match these filters.
              </p>
            {:else}
              <div class="repo-grid">
                {#each filteredLocals as r (r.id)}
                  {@render repoCardEntry(r)}
                {/each}
              </div>
            {/if}
          {/if}

          {#if error}
            <p class="err-banner">{error}</p>
          {/if}
        {/if}
      </main>

      {#if detailPaneOpen && selectedRepo}
        {@const r = selectedRepo}
        {@const fullName = repoFullName(r)}
        {@const localDiag = selectedLocalDiag[0]}
        {@const editorCmd = selectedEditorCmd}
        <aside class="detail-pane" aria-label="Repo details">
          <header class="dp-header">
            <span class="pchip dp-pchip {providerCssClass(r.provider)}">{providerChipText(r)}</span>
            <div class="dp-titles">
              <h2 class="dp-name">
                <span class="dp-owner">{r.owner}/</span><span class="dp-rname">{r.name}</span>
              </h2>
              <div class="dp-meta">
                <span>{providerLabel({ provider: r.provider, html_url: r.html_url })}</span>
                {#if r.is_private}<span class="dp-badge">private</span>{/if}
                {#if r.is_fork}<span class="dp-badge">fork</span>{/if}
                <span class="dp-branch">{r.default_branch}</span>
              </div>
            </div>
            <button
              type="button"
              class="dp-close"
              onclick={() => (selectedRepo = null)}
              aria-label="Close detail pane"
              data-tip="Close (Esc)"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
                <path d="M18 6 6 18" />
                <path d="m6 6 12 12" />
              </svg>
            </button>
          </header>

          {#if r.description}
            <p class="dp-desc">{r.description}</p>
          {/if}

          <div class="dp-actions">
            <button
              type="button"
              class="dp-action primary"
              onclick={() => openExternal(r.html_url)}
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
                <path d="M7 17 17 7" />
                <path d="M7 7h10v10" />
              </svg>
              Open in browser
            </button>
            {#if localDiag}
              <button
                type="button"
                class="dp-action"
                onclick={() => revealItemInDir(localDiag.path)}
                data-tip="Reveal in Finder"
              >
                Show in Finder
              </button>
              {#if editorCmd.length > 0}
                <button
                  type="button"
                  class="dp-action"
                  onclick={() => runEditor(localDiag.path)}
                  data-tip="Open with {editorCmd}"
                >
                  Open in {editorCmd}
                </button>
              {/if}
            {/if}
            {#if r.clone_url}
              {@const cloneUrl = r.clone_url}
              <button
                type="button"
                class="dp-action"
                onclick={() => writeText(cloneUrl)}
                data-tip="Copy to clipboard"
              >
                Copy HTTPS
              </button>
            {/if}
            {#if r.ssh_url}
              {@const sshUrl = r.ssh_url}
              <button
                type="button"
                class="dp-action"
                onclick={() => writeText(sshUrl)}
                data-tip="Copy to clipboard"
              >
                Copy SSH
              </button>
            {/if}
          </div>

          <section class="dp-sec">
            <h3 class="dp-sec-h">Clone</h3>
            {#if selectedLocalDiag.length === 0}
              <p class="dp-empty">
                Not cloned locally.
                {#if settings.scan_roots.length === 0}
                  Add a folder in <button
                    type="button"
                    class="link-inline"
                    onclick={() => (view = 'settings')}
                  >Settings</button> for gitBuddy to find local copies.
                {/if}
              </p>
            {:else}
              {#each selectedLocalDiag as l (l.path)}
                {@const dirty = l.dirty_staged + l.dirty_unstaged}
                <div class="dp-clone">
                  <div class="dp-clone-path" title={l.path}>{l.path}</div>
                  <div class="dp-clone-row">
                    <span class="dp-clone-branch">
                      <span class="d" class:off={l.detached}></span>
                      {l.branch ?? (l.detached ? 'detached' : '—')}
                    </span>
                    {#if l.ahead > 0}<span class="dp-clone-stat warn">{l.ahead} ahead</span>{/if}
                    {#if l.behind > 0}<span class="dp-clone-stat warn">{l.behind} behind</span>{/if}
                    {#if dirty > 0}<span class="dp-clone-stat warn">{dirty} uncommitted</span>{/if}
                    {#if l.untracked > 0}<span class="dp-clone-stat">{l.untracked} untracked</span>{/if}
                    {#if l.ahead === 0 && l.behind === 0 && dirty === 0 && l.untracked === 0}
                      <span class="dp-clone-stat clean">clean</span>
                    {/if}
                  </div>
                </div>
              {/each}
            {/if}
          </section>

          <section class="dp-sec">
            <h3 class="dp-sec-h">CI</h3>
            {#if selectedCi === null || selectedCi.status === 'none'}
              <p class="dp-empty">No recent workflow runs.</p>
            {:else}
              {@const ci = selectedCi}
              <div class="dp-ci">
                <span class="rci {ci.status}">
                  <span class="b"></span>
                  {#if ci.status === 'ok'}passing
                  {:else if ci.status === 'fail'}failing
                  {:else if ci.status === 'run'}running
                  {:else if ci.status === 'cancelled'}cancelled
                  {/if}
                </span>
                <span class="dp-ci-meta">
                  {ci.workflow_name ?? 'workflow'}
                  {#if ci.branch}<span class="dp-ci-branch">on {ci.branch}</span>{/if}
                </span>
                {#if ci.html_url}
                  {@const ciUrl = ci.html_url}
                  <button
                    type="button"
                    class="dp-link"
                    onclick={() => openExternal(ciUrl)}
                  >View run →</button>
                {/if}
              </div>
            {/if}
          </section>

          <section class="dp-sec">
            <h3 class="dp-sec-h">Latest release</h3>
            {#if selectedRelease === null}
              <p class="dp-empty">No releases published.</p>
            {:else}
              {@const rel = selectedRelease}
              <div class="dp-release">
                <div class="dp-release-title">
                  <span class="dp-release-name">{rel.name || rel.tag}</span>
                  {#if rel.is_prerelease}<span class="badge-pre">pre</span>{/if}
                  {#if rel.is_new}<span class="new-badge">NEW</span>{/if}
                </div>
                <div class="dp-release-meta">
                  <span class="row-tag">{rel.tag}</span>
                  <span class="row-dot">·</span>
                  <span>{rel.age_human}</span>
                  <button
                    type="button"
                    class="dp-link"
                    onclick={() => openExternal(rel.html_url)}
                  >View release →</button>
                </div>
              </div>
            {/if}
          </section>

          <section class="dp-sec">
            <h3 class="dp-sec-h">
              Waiting on you
              {#if selectedItems.length > 0}
                <span class="dp-sec-count">{selectedItems.length}</span>
              {/if}
            </h3>
            {#if selectedItems.length === 0}
              <p class="dp-empty">Nothing waiting in this repo.</p>
            {:else}
              <div class="dp-items">
                {#each selectedItems as it (it.id)}
                  <button
                    type="button"
                    class="row dp-item"
                    onclick={() => openExternal(it.url)}
                    oncontextmenu={(e) => openItemMenu(e, it)}
                  >
                    <span class="kind-chip {it.kind.toLowerCase()}">{it.kind}</span>
                    <span class="row-body">
                      <span class="row-title">{it.title}</span>
                      <span class="row-meta">
                        <span class="row-reason">{it.reason === 'review' ? 'review requested' : it.reason}</span>
                      </span>
                    </span>
                    <span class="row-age">{it.age_human}</span>
                  </button>
                {/each}
              </div>
            {/if}
          </section>
        </aside>
      {/if}
    </div>
  {:else}
    <!-- ─────────── Settings ─────────── -->
    <main class="settings">
      <div class="settings-inner">
        <h1 class="settings-title">Settings</h1>

        <!-- Connected providers -->
        <section class="set-sec">
          <h3>Connected <em>accounts</em></h3>
          {#if accounts.length === 0}
            <p class="set-empty">No accounts yet — add one below.</p>
          {:else}
            <ul class="prov-list">
              {#each accounts as account (account.id)}
                {@const acctHost = account.base_url
                  ? (() => {
                      try {
                        return new URL(account.base_url!).host;
                      } catch {
                        return account.base_url!;
                      }
                    })()
                  : 'github.com'}
                {@const chipText = providerChipText({
                  provider: account.provider,
                  html_url: account.base_url ?? '',
                })}
                {@const chipClass = providerCssClass(account.provider)}
                <li class="prov-row">
                  <span class="pchip {chipClass}">{chipText}</span>
                  <div class="prov-meta">
                    <div class="prov-name">
                      {account.viewer.name ?? account.login}
                      {#if account.auth === 'oauth_device'}
                        <span class="prov-auth-badge" data-tip="OAuth Device Flow"
                          >oauth</span>
                      {/if}
                    </div>
                    <div class="prov-host">{acctHost}</div>
                  </div>
                  <button
                    type="button"
                    class="prov-disconnect"
                    onclick={() => disconnectAccount(account)}
                  >
                    Disconnect
                  </button>
                </li>
              {/each}
            </ul>
          {/if}

          {#if !addingProvider}
            <button type="button" class="set-add" onclick={startAddingProvider}>
              + Add account…
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

              {#if chosenProvider === 'github'}
                <div class="auth-method">
                  <button
                    type="button"
                    class:on={githubAuthMethod === 'oauth'}
                    onclick={() => {
                      githubAuthMethod = 'oauth';
                      resetOAuthState();
                    }}
                    disabled={connecting || oauthState === 'awaiting'}
                  >
                    Sign in with browser
                  </button>
                  <button
                    type="button"
                    class:on={githubAuthMethod === 'pat'}
                    onclick={() => {
                      githubAuthMethod = 'pat';
                      resetOAuthState();
                    }}
                    disabled={connecting || oauthState === 'awaiting'}
                  >
                    Personal access token
                  </button>
                </div>

                {#if githubAuthMethod === 'oauth'}
                  {#if oauthState === 'idle'}
                    <p class="oauth-blurb">
                      Open <em>GitHub</em> in your browser, paste a short code,
                      done. No token-management page to navigate.
                    </p>
                    <button
                      type="button"
                      class="primary oauth-start"
                      onclick={startGithubOAuth}
                      disabled={connecting}
                    >
                      {connecting ? 'Contacting GitHub…' : 'Sign in with browser'}
                    </button>
                  {:else if oauthState === 'awaiting'}
                    <div class="oauth-flight">
                      <p class="oauth-step">
                        Enter this code at <em>github.com/login/device</em>:
                      </p>
                      <div class="oauth-code-row">
                        <span class="oauth-code">{oauthUserCode}</span>
                        <button
                          type="button"
                          class="oauth-copy"
                          onclick={copyUserCode}
                          data-tip="Copy code"
                          aria-label="Copy code"
                        >
                          {oauthCopied ? 'Copied' : 'Copy'}
                        </button>
                      </div>

                      <div class="oauth-progress" aria-hidden="true">
                        <div
                          class="oauth-progress-bar"
                          style="width: {oauthExpiresIn > 0
                            ? Math.max(0, (oauthRemaining / oauthExpiresIn) * 100)
                            : 0}%"
                        ></div>
                      </div>
                      <p class="oauth-meta">
                        <span class="oauth-spinner" aria-hidden="true"></span>
                        Waiting for approval —
                        {Math.floor(oauthRemaining / 60)}m {oauthRemaining % 60}s left
                      </p>

                      <button
                        type="button"
                        class="oauth-fallback-link"
                        onclick={() => openExternal(oauthVerificationUri)}
                      >
                        Open github.com/login/device again →
                      </button>
                    </div>
                  {:else if oauthState === 'error'}
                    <div class="oauth-error">
                      <p class="err">{oauthErrorMsg}</p>
                      <button
                        type="button"
                        class="primary"
                        onclick={startGithubOAuth}
                      >
                        Try again
                      </button>
                    </div>
                  {/if}
                {:else}
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
                {/if}
              {:else if chosenProvider === 'gitlab'}
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
              {:else if chosenProvider === 'codeberg'}
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
                {#if !(chosenProvider === 'github' && githubAuthMethod === 'oauth')}
                  <button
                    type="button"
                    class="primary"
                    onclick={connectProvider}
                    disabled={connecting || !tokenInput.trim() || (chosenProvider === 'gitlab' && !gitlabBaseInput.trim()) || (chosenProvider === 'codeberg' && !codebergBaseInput.trim())}
                  >
                    {connecting ? 'Verifying…' : 'Connect'}
                  </button>
                {/if}
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
  .body.has-detail {
    grid-template-columns: 240px minmax(0, 1.4fr) minmax(360px, 1fr);
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
  .pill-btn {
    border: 0;
    background: transparent;
    font: inherit;
    cursor: pointer;
    transition: background 0.12s ease;
  }
  .pill-btn:hover:not(.on) { background: var(--cream-2); }
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
  .pill.muted { opacity: 0.45; }
  .pill.muted .c { text-decoration: line-through; }
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

  /* Account pill = main toggle button + small "open in browser" button. */
  .acct-pill {
    padding: 0;
    gap: 0;
  }
  .acct-toggle {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 6px 9px 12px;
    background: transparent;
    border: 0;
    font: inherit;
    color: inherit;
    cursor: pointer;
    border-radius: var(--r-md);
    text-align: left;
    transition: background 0.12s ease;
  }
  .acct-toggle:hover { background: var(--cream-2); }
  .acct-open {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    margin-right: 6px;
    border-radius: var(--r-sm);
    background: transparent;
    border: 0;
    color: var(--ink-3);
    cursor: pointer;
    flex-shrink: 0;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .acct-open:hover {
    background: var(--cream-2);
    color: var(--terracotta);
  }
  .pill.muted .acct-open { opacity: 0.75; }

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

  /* Stack of provider/host chips on a repo card — appears in place of the
     single .pchip when more than one account is connected, so the user can
     see which of their accounts surface this repo. */
  .acct-badges {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 3px;
  }

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

  /* Search clear button -------------------------------------------- */
  .search-clear {
    width: 22px;
    height: 22px;
    display: grid;
    place-items: center;
    border-radius: 50%;
    border: 0;
    background: var(--cream-2);
    color: var(--ink-3);
    font-size: 15px;
    line-height: 1;
    cursor: pointer;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .search-clear:hover {
    background: var(--terracotta-soft);
    color: var(--terracotta);
  }

  /* Reason chips row ---------------------------------------------- */
  .reason-chips {
    padding: 10px 18px 14px;
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--paper-2);
    border-bottom: 1px solid var(--line);
    flex-wrap: wrap;
  }
  .reason-chips-label {
    font-family: var(--font-mono);
    font-size: 11px;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--ink-3);
    margin-right: 4px;
  }
  .chip-toggle {
    height: 26px;
    padding: 0 12px;
    border-radius: 999px;
    border: 1px solid var(--line-2);
    background: var(--paper);
    color: var(--ink-3);
    font-size: 12px;
    font-family: var(--font-mono);
    cursor: pointer;
    text-transform: capitalize;
    transition: background 0.12s ease, color 0.12s ease, border-color 0.12s ease, opacity 0.12s ease;
  }
  .chip-toggle:hover { border-color: var(--terracotta); color: var(--terracotta); }
  .chip-toggle.on {
    background: var(--terracotta-soft);
    color: var(--terracotta);
    border-color: transparent;
    font-weight: 600;
  }
  .chip-toggle:not(.on) {
    opacity: 0.55;
    text-decoration: line-through;
  }

  /* section-h secondary count ------------------------------------- */
  .section-h .muted-count {
    color: var(--ink-4);
  }

  /* Item / Release row list --------------------------------------- */
  .row-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .row {
    width: 100%;
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 12px 16px;
    display: grid;
    grid-template-columns: 36px 1fr auto;
    gap: 14px;
    align-items: center;
    text-align: left;
    cursor: pointer;
    transition: transform 0.12s ease, box-shadow 0.12s ease;
  }
  .row:hover {
    transform: translateY(-1px);
    box-shadow: var(--shadow-2);
  }
  .kind-chip {
    width: 30px;
    height: 30px;
    border-radius: 9px;
    display: grid;
    place-items: center;
    font-family: var(--font-display);
    font-size: 11px;
    font-weight: 700;
    color: white;
    flex-shrink: 0;
  }
  .kind-chip.pr { background: linear-gradient(135deg, #80987B, #4A5E48); }
  .kind-chip.mr { background: linear-gradient(135deg, #E8A06A, #C66243); }
  .kind-chip.is { background: linear-gradient(135deg, #B6A5C9, #6E5E80); }
  .kind-chip.rel {
    background: linear-gradient(135deg, #F1D58A, #B68C2C);
    color: var(--ink);
  }
  .row-body {
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .row-title {
    font-weight: 500;
    font-size: 14px;
    color: var(--ink);
    line-height: 1.3;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .row-meta {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .row-dot { color: var(--ink-4); }
  .row-repo { color: var(--ink-2); }
  .row-reason { color: var(--terracotta); text-transform: lowercase; }
  .row-prov { color: var(--ink-3); }
  .row-tag {
    background: var(--cream-2);
    padding: 1px 6px;
    border-radius: 4px;
    color: var(--ink-2);
  }
  .row-age {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
    display: inline-flex;
    align-items: center;
    gap: 6px;
    white-space: nowrap;
  }
  .badge-pre {
    background: var(--plum-soft);
    color: var(--plum);
    font-family: var(--font-mono);
    font-size: 10px;
    padding: 1px 6px;
    border-radius: 4px;
    margin-left: 6px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .new-badge {
    background: var(--terracotta);
    color: white;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 700;
    padding: 2px 6px;
    border-radius: 4px;
    letter-spacing: 0.04em;
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
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .prov-auth-badge {
    font-family: var(--font-mono);
    font-size: 10px;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--sage);
    background: var(--sage-soft);
    padding: 1px 6px;
    border-radius: 4px;
    font-weight: 600;
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

  /* ── GitHub OAuth Device Flow ────────────────────────────────────── */
  .auth-method {
    display: flex;
    gap: 4px;
    padding: 4px;
    background: var(--cream-2);
    border-radius: var(--r-sm);
    font-size: 12.5px;
  }
  .auth-method button {
    flex: 1;
    padding: 6px 10px;
    color: var(--ink-2);
    border-radius: 6px;
    background: transparent;
    border: 0;
    cursor: pointer;
  }
  .auth-method button.on {
    background: var(--paper);
    color: var(--ink);
    font-weight: 600;
    box-shadow: var(--shadow-1);
  }
  .auth-method button:disabled { cursor: default; opacity: 0.6; }

  .oauth-blurb {
    margin: 0;
    color: var(--ink-2);
    font-size: 13px;
    line-height: 1.45;
  }
  .oauth-blurb em {
    font-family: var(--font-display);
    font-style: italic;
    color: var(--terracotta);
  }
  .oauth-start { align-self: flex-start; }

  .oauth-flight {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  .oauth-step {
    margin: 0;
    color: var(--ink-2);
    font-size: 13px;
  }
  .oauth-step em {
    font-family: var(--font-mono);
    font-style: normal;
    color: var(--ink);
    font-size: 12.5px;
    background: var(--cream-2);
    padding: 1px 6px;
    border-radius: 4px;
  }
  .oauth-code-row {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .oauth-code {
    flex: 1;
    text-align: center;
    font-family: var(--font-mono);
    font-size: 28px;
    letter-spacing: 0.18em;
    color: var(--terracotta);
    background: var(--cream-2);
    padding: 16px 20px;
    border-radius: 12px;
    user-select: all;
  }
  .oauth-copy {
    height: 36px;
    padding: 0 14px;
    background: transparent;
    border: 1px solid var(--line-2);
    color: var(--ink-2);
    border-radius: var(--r-sm);
    font-size: 12.5px;
    cursor: pointer;
    transition: border-color 0.15s, color 0.15s, background 0.15s;
  }
  .oauth-copy:hover {
    border-color: var(--terracotta);
    color: var(--terracotta);
  }

  .oauth-progress {
    height: 4px;
    background: var(--cream-2);
    border-radius: 999px;
    overflow: hidden;
  }
  .oauth-progress-bar {
    height: 100%;
    background: var(--sage);
    transition: width 1s linear;
  }
  .oauth-meta {
    margin: 0;
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--ink-3);
    font-size: 12px;
    font-family: var(--font-mono);
  }
  .oauth-spinner {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    border: 1.5px solid var(--cream-2);
    border-top-color: var(--terracotta);
    animation: oauth-spin 0.9s linear infinite;
  }
  @keyframes oauth-spin {
    to { transform: rotate(360deg); }
  }
  .oauth-fallback-link {
    align-self: flex-start;
    background: transparent;
    border: 0;
    padding: 0;
    color: var(--ink-3);
    font-size: 12px;
    cursor: pointer;
  }
  .oauth-fallback-link:hover { color: var(--terracotta); }

  .oauth-error {
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .oauth-error .err {
    margin: 0;
    color: var(--terracotta);
    font-size: 13px;
    line-height: 1.45;
  }
  .oauth-error .primary { align-self: flex-start; }

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
  @media (max-width: 1024px) {
    .body.has-detail {
      grid-template-columns: minmax(0, 1.4fr) minmax(360px, 1fr);
    }
    .body.has-detail .side { display: none; }
  }
  @media (max-width: 720px) {
    .body { grid-template-columns: 1fr; }
    .body.has-detail { grid-template-columns: 1fr; }
    .body.has-detail .content { display: none; }
    .side { display: none; }
    .stats { grid-template-columns: repeat(2, 1fr); }
    .repo-grid { grid-template-columns: 1fr; }
  }

  /* Detail pane --------------------------------------------------- */
  .detail-pane {
    border-left: 1px solid var(--line);
    background: var(--paper-2);
    padding: 22px 24px 26px;
    overflow-y: auto;
    overflow-x: hidden;
    display: flex;
    flex-direction: column;
    gap: 18px;
    animation: dp-in 0.18s ease-out;
  }
  @keyframes dp-in {
    from {
      opacity: 0;
      transform: translateX(8px);
    }
    to {
      opacity: 1;
      transform: translateX(0);
    }
  }

  .card.selected {
    background: var(--terracotta-soft);
    border-color: rgba(198, 98, 67, 0.22);
    box-shadow: var(--shadow-2);
  }
  .card.selected:hover {
    transform: none;
  }

  .dp-header {
    display: grid;
    grid-template-columns: 36px 1fr auto;
    gap: 12px;
    align-items: start;
  }
  .dp-pchip {
    width: 36px;
    height: 36px;
    border-radius: 10px;
    font-size: 13px;
  }
  .dp-titles {
    min-width: 0;
  }
  .dp-name {
    font-family: var(--font-display);
    font-size: 22px;
    font-weight: 400;
    letter-spacing: -0.02em;
    color: var(--ink);
    margin: 0 0 4px;
    line-height: 1.15;
    word-break: break-word;
  }
  .dp-owner { color: var(--ink-3); }
  .dp-rname { font-weight: 600; }
  .dp-meta {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .dp-badge {
    background: var(--cream-2);
    color: var(--ink-2);
    padding: 1px 6px;
    border-radius: 4px;
  }
  .dp-branch {
    background: var(--cream-2);
    color: var(--ink-2);
    padding: 1px 6px;
    border-radius: 4px;
  }
  .dp-close {
    width: 28px;
    height: 28px;
    border-radius: var(--r-sm);
    border: 0;
    background: transparent;
    color: var(--ink-3);
    cursor: pointer;
    display: grid;
    place-items: center;
    transition: background 0.12s ease, color 0.12s ease;
  }
  .dp-close:hover {
    background: var(--cream-2);
    color: var(--terracotta);
  }

  .dp-desc {
    margin: 0;
    font-size: 13.5px;
    color: var(--ink-2);
    line-height: 1.55;
  }

  .dp-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .dp-action {
    height: 30px;
    padding: 0 12px;
    background: var(--paper);
    border: 1px solid var(--line-2);
    border-radius: var(--r-sm);
    font-size: 12.5px;
    color: var(--ink-2);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 5px;
    transition: background 0.12s ease, color 0.12s ease, border-color 0.12s ease;
  }
  .dp-action:hover {
    background: var(--cream-2);
    border-color: var(--terracotta);
    color: var(--terracotta);
  }
  .dp-action.primary {
    background: var(--terracotta);
    color: white;
    border-color: transparent;
  }
  .dp-action.primary:hover {
    background: #B05738;
    color: white;
    border-color: transparent;
  }

  .dp-sec {
    border-top: 1px solid var(--line);
    padding-top: 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dp-sec-h {
    font-family: var(--font-display);
    font-size: 13px;
    font-weight: 400;
    color: var(--ink-3);
    letter-spacing: 0.04em;
    text-transform: uppercase;
    margin: 0;
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .dp-sec-count {
    font-family: var(--font-mono);
    font-size: 11px;
    background: var(--terracotta-soft);
    color: var(--terracotta);
    padding: 1px 7px;
    border-radius: 999px;
    text-transform: none;
    letter-spacing: 0;
    font-weight: 600;
  }
  .dp-empty {
    margin: 0;
    color: var(--ink-3);
    font-size: 12.5px;
    font-style: italic;
  }

  .dp-clone {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dp-clone + .dp-clone { margin-top: 8px; }
  .dp-clone-path {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-2);
    background: var(--paper);
    border: 1px solid var(--line);
    border-radius: var(--r-sm);
    padding: 6px 10px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dp-clone-row {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
  }
  .dp-clone-branch {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    color: var(--ink-2);
  }
  .dp-clone-branch .d {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sage);
  }
  .dp-clone-branch .d.off { background: var(--ink-4); }
  .dp-clone-stat { color: var(--ink-3); }
  .dp-clone-stat.warn { color: var(--terracotta); }
  .dp-clone-stat.clean { color: var(--sage); }

  .dp-ci {
    display: flex;
    align-items: center;
    gap: 10px;
    flex-wrap: wrap;
  }
  .dp-ci-meta {
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-2);
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .dp-ci-branch {
    color: var(--ink-3);
  }

  .dp-release {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dp-release-title {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .dp-release-name {
    font-size: 14px;
    color: var(--ink);
    font-weight: 500;
  }
  .dp-release-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
    flex-wrap: wrap;
  }

  .dp-link {
    background: transparent;
    border: 0;
    padding: 0;
    color: var(--terracotta);
    font-size: 12px;
    cursor: pointer;
    margin-left: auto;
  }
  .dp-link:hover { text-decoration: underline; }

  .dp-items {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .dp-item {
    padding: 10px 12px;
    grid-template-columns: 28px 1fr auto;
    gap: 10px;
  }
  .dp-item .kind-chip {
    width: 26px;
    height: 26px;
    font-size: 10px;
  }
  .dp-item .row-title {
    font-size: 13px;
    -webkit-line-clamp: 2;
    line-clamp: 2;
  }
</style>
