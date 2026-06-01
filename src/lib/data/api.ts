// Tauri command bindings + shared types. Type shapes mirror Rust's
// src-tauri/src/types.rs — see the serde rename rules there for the exact
// wire format.

import { invoke } from '@tauri-apps/api/core';

export type Provider = 'github' | 'gitlab' | 'codeberg' | 'mpsd-gitlab';
export type ItemKind = 'PR' | 'MR' | 'IS';
export type ItemReason = 'assigned' | 'review' | 'authored' | 'mentioned';

export interface WaitingItem {
  id: string;
  kind: ItemKind;
  title: string;
  repo: string;
  provider: Provider;
  reason: ItemReason;
  url: string;
  age_human: string;
  updated_at: string;
  /** `Account.id` of the account that surfaced this item. Always set on
   *  results from `list_waiting`; only nullable because Rust's struct
   *  literal needs a default during construction. */
  account_id: string | null;
}

export interface Viewer {
  login: string;
  avatar_url: string | null;
  name: string | null;
}

export interface GitLabStatus {
  viewer: Viewer;
  base_url: string;
}

export interface CodebergStatus {
  viewer: Viewer;
  base_url: string;
}

export interface Repo {
  id: string;
  owner: string;
  name: string;
  provider: Provider;
  default_branch: string;
  language: string | null;
  description: string | null;
  stars: number;
  html_url: string;
  ssh_url: string | null;
  clone_url: string | null;
  is_fork: boolean;
  is_private: boolean;
  pushed_at: string | null;
  /** `Account.id` of the account that surfaced this repo. The aggregator
   *  in `list_repos` returns one row per (account, repo) pair; the UI
   *  dedups them so a single repo visible to multiple accounts collapses
   *  to one row with badges for each account. */
  account_id: string | null;
}

/** Short display label for the provider, derived from the canonical URL on
 *  the item so self-hosted GitLab instances show their actual hostname
 *  (e.g. "gitlab.gwdg.de") rather than a stub "MPSD" placeholder. */
export function providerLabel(item: { provider: Provider; url?: string; html_url?: string }): string {
  switch (item.provider) {
    case 'github':
      return 'GitHub';
    case 'gitlab':
      return 'GitLab';
    case 'codeberg':
      return 'Codeberg';
    case 'mpsd-gitlab':
      return extractHost(item.url ?? item.html_url ?? '') || 'GitLab';
  }
}

/** Two-character chip text. For self-hosted GitLab we derive a slug from the
 *  hostname (e.g. "gitlab.gwdg.de" → "gw", "gitlab.mpsd.mpg.de" → "mp") so
 *  the user can tell different instances apart at a glance. */
export function providerChipText(item: { provider: Provider; url?: string; html_url?: string }): string {
  switch (item.provider) {
    case 'github':
      return 'gh';
    case 'gitlab':
      return 'gl';
    case 'codeberg':
      return 'cb';
    case 'mpsd-gitlab': {
      const host = extractHost(item.url ?? item.html_url ?? '');
      return shortHostSlug(host);
    }
  }
}

/** CSS class to colour the chip. Self-hosted GitLab keeps a plum tint so it
 *  reads as "GitLab, but not the .com one". */
export function providerCssClass(provider: Provider): string {
  switch (provider) {
    case 'github':
      return 'gh';
    case 'gitlab':
      return 'gl';
    case 'codeberg':
      return 'cb';
    case 'mpsd-gitlab':
      return 'gl-self';
  }
}

function extractHost(url: string): string {
  try {
    return new URL(url).host;
  } catch {
    return '';
  }
}

/** "gitlab.gwdg.de" → "gw", "gitlab.mpsd.mpg.de" → "mp", "git.example.com" → "ex". */
function shortHostSlug(host: string): string {
  if (!host) return 'gl';
  const parts = host.split('.');
  // Skip a leading "gitlab" / "git" / "code" subdomain so the slug reflects
  // the instance owner rather than the product name.
  const skip = new Set(['gitlab', 'git', 'code']);
  const idx = parts[0] && skip.has(parts[0].toLowerCase()) ? 1 : 0;
  const seg = parts[idx] ?? 'gl';
  return seg.slice(0, 2).toLowerCase();
}

/** Fallback host portion when a Repo doesn't have a usable html_url. Only
 *  applies to gitlab.com / github.com / codeberg.org — for self-hosted
 *  instances the actual host has to come from html_url because we don't
 *  know it ahead of time. */
export const providerHost: Record<Provider, string> = {
  github: 'github.com',
  gitlab: 'gitlab.com',
  codeberg: 'codeberg.org',
  'mpsd-gitlab': '',
};

export interface RemoteRef {
  host: string;
  owner: string;
  name: string;
  raw_url: string;
}

export interface LocalRepo {
  path: string;
  branch: string | null;
  remote: RemoteRef | null;
  dirty_staged: number;
  dirty_unstaged: number;
  untracked: number;
  ahead: number;
  behind: number;
  detached: boolean;
}

/** Notification settings. Three independently-toggleable gates, in
 *  decreasing scope: master switch (`enabled`) → Do-Not-Disturb (a quick
 *  silence that preserves per-event preferences) → per-event toggles. */
export interface NotificationSettings {
  enabled: boolean;
  do_not_disturb: boolean;
  events: NotificationEventToggles;
}

export interface NotificationEventToggles {
  /** Issue/PR/MR assigned, review-requested, mentioned, or authored. */
  waiting: boolean;
  /** New release published in a repo the account has access to. */
  releases: boolean;
  /** CI run failed and was triggered by the viewer (Phase 3 — the
   *  toggle persists today but the diff that drives it lands later). */
  ci_failure: boolean;
}

/** Persistent user settings. Schema v2 (M6.5+). The on-disk file is
 *  silently migrated from v1 by the Rust loader on first launch after an
 *  upgrade, so the frontend doesn't need to know about the older shape. */
export interface Settings {
  version: number;
  scan_roots: string[];
  scan_ignore: string[];
  gitlab_base_url: string | null;
  codeberg_base_url: string | null;
  /** Shell command spawned by "Open in editor" — repo path is appended.
   *  Empty/null disables that quick-action menu entry. */
  editor_command: string | null;
  notifications: NotificationSettings;
  /** Aggregator polling cadence in minutes. Clamped backend-side to
   *  `[1, 60]`; the UI should also enforce that band so a user can't
   *  drag the slider to a silently-corrected value. */
  poll_interval_minutes: number;
}

/** Minimum / maximum / default for `poll_interval_minutes`, mirrored
 *  from the Rust `settings` module so the slider stays in sync without
 *  another round-trip. */
export const POLL_INTERVAL_MIN = 1;
export const POLL_INTERVAL_MAX = 60;
export const POLL_INTERVAL_DEFAULT = 5;

/** A fresh v2 Settings object with backend defaults. Both windows seed their
 *  `settings` state with this before the first `getSettings()` resolves, so
 *  the shape lives here instead of being copy-pasted into each route. */
export function defaultSettings(): Settings {
  return {
    version: 2,
    scan_roots: [],
    scan_ignore: [],
    gitlab_base_url: null,
    codeberg_base_url: null,
    editor_command: null,
    notifications: {
      enabled: true,
      do_not_disturb: false,
      events: { waiting: true, releases: true, ci_failure: true },
    },
    poll_interval_minutes: POLL_INTERVAL_DEFAULT,
  };
}

export interface Release {
  repo_id: string;
  repo_full_name: string;
  provider: Provider;
  tag: string;
  name: string;
  published_at: string;
  html_url: string;
  is_prerelease: boolean;
  is_new: boolean;
  age_human: string;
  account_id: string | null;
}

export type CiStatus = 'ok' | 'fail' | 'run' | 'cancelled' | 'none';

export interface CiRun {
  repo_id: string;
  repo_full_name: string;
  status: CiStatus;
  html_url: string | null;
  branch: string | null;
  workflow_name: string | null;
  /** Login of the user that triggered this run. Used backend-side to gate
   *  CI-failure notifications to the viewer-as-author. `null` for self-
   *  hosted Forgejo instances that don't expose an actor. */
  author_login: string | null;
  account_id: string | null;
}

// ── Tauri commands ─────────────────────────────────────────────────────────

// ── Per-provider auth ──────────────────────────────────────────────────────

/** Backend `commands::ProviderStatus`. One unified `provider_status` command
 *  replaced the gh_/gl_/cb_status trio; `base_url` is null for GitHub. */
export interface ProviderStatus {
  viewer: Viewer;
  base_url: string | null;
}

const providerStatus = (provider: Provider): Promise<ProviderStatus | null> =>
  invoke('provider_status', { provider });

export const ghStatus = (): Promise<Viewer | null> =>
  providerStatus('github').then((s) => s?.viewer ?? null);
export const ghSetToken = (token: string): Promise<Viewer> =>
  invoke('provider_set_token', { provider: 'github', token, baseUrl: null });

export const ghDisconnect = (): Promise<void> =>
  invoke('provider_disconnect', { provider: 'github' });

// ── GitHub OAuth Device Flow (M6.3) ────────────────────────────────────────

export interface DeviceCodeResponse {
  device_code: string;
  user_code: string;
  verification_uri: string;
  expires_in: number;
  interval: number;
}

/** Tagged union mirroring src-tauri/src/commands.rs::GhOAuthPollResult.
 *  The backend only emits one of these five variants per poll. */
export type GhOAuthPollResult =
  | { kind: 'success'; viewer: Viewer }
  | { kind: 'pending' }
  | { kind: 'slow_down'; interval: number }
  | { kind: 'denied' }
  | { kind: 'expired' };

/** Start the GitHub OAuth Device Flow. Returns the user_code for the human
 *  plus the device_code + interval the caller echoes back into ghOAuthPoll. */
export const ghOAuthBegin = (): Promise<DeviceCodeResponse> =>
  invoke('gh_oauth_begin');

/** One Device Flow poll. The frontend drives the cadence — defaults to the
 *  `interval` field from ghOAuthBegin and bumps on `slow_down`. */
export const ghOAuthPoll = (deviceCode: string): Promise<GhOAuthPollResult> =>
  invoke('gh_oauth_poll', { deviceCode });

// ── Multi-account registry ────────────────────────────────────────────────

export type AuthMethod = 'pat' | 'oauth_device';

export interface Account {
  /** Stable identifier `<provider>:<host>:<login>` — also the Keychain key. */
  id: string;
  provider: Provider;
  login: string;
  viewer: Viewer;
  auth: AuthMethod;
  /** `null` for GitHub.com, set for GitLab/Codeberg/Gitea instances. */
  base_url: string | null;
  /** RFC 3339 timestamp captured at first connect. */
  added_at: string;
}

/** Every connected account, regardless of provider. Source of truth for the
 *  Settings UI; supersedes the legacy single-account ghStatus / glStatus /
 *  cbStatus trio. */
export const accountsList = (): Promise<Account[]> => invoke('accounts_list');

/** Disconnect a single account by id — removes it from the in-memory
 *  HashMap, deletes its Keychain entry, and drops the accounts.json record. */
export const accountsDisconnect = (accountId: string): Promise<void> =>
  invoke('accounts_disconnect', { accountId });

export const glStatus = (): Promise<GitLabStatus | null> =>
  providerStatus('gitlab').then((s) =>
    s ? { viewer: s.viewer, base_url: s.base_url ?? '' } : null,
  );
export const glSetToken = (token: string, baseUrl: string): Promise<Viewer> =>
  invoke('provider_set_token', { provider: 'gitlab', token, baseUrl });
export const glDisconnect = (): Promise<void> =>
  invoke('provider_disconnect', { provider: 'gitlab' });

export const cbStatus = (): Promise<CodebergStatus | null> =>
  providerStatus('codeberg').then((s) =>
    s ? { viewer: s.viewer, base_url: s.base_url ?? '' } : null,
  );
export const cbSetToken = (token: string, baseUrl: string): Promise<Viewer> =>
  invoke('provider_set_token', { provider: 'codeberg', token, baseUrl });
export const cbDisconnect = (): Promise<void> =>
  invoke('provider_disconnect', { provider: 'codeberg' });

/** Reveal the main window. */
export const openMainWindow = (): Promise<void> => invoke('open_main');

/** Reveal the main window and navigate it to its Settings view. The popover
 *  forwards its gear-icon click here — settings live in the main window so
 *  they have room to breathe. */
export const openMainSettings = (): Promise<void> => invoke('open_main_settings');

// ── Aggregated data (across all connected providers) ───────────────────────

/** Items where the user is assigned, review-requested, authored, or mentioned. */
export const listWaiting = (): Promise<WaitingItem[]> => invoke('list_waiting');

/** All repos visible to any connected provider. */
export const listRepos = (): Promise<Repo[]> => invoke('list_repos');

/** Latest release per recently-active repo. GitHub only for now. */
export const listReleases = (): Promise<Release[]> => invoke('list_releases');

/** Latest CI workflow run on each repo's default branch. GitHub only for now. */
export const listCi = (): Promise<CiRun[]> => invoke('list_ci');

/** Scan configured roots and report every local checkout with diagnostics. */
export const listLocalRepos = (): Promise<LocalRepo[]> => invoke('list_local_repos');

/** Load persisted user settings (scan roots, ignore patterns). */
export const getSettings = (): Promise<Settings> => invoke('get_settings');

/** Persist user settings to the OS config directory. */
export const saveSettings = (settings: Settings): Promise<void> =>
  invoke('save_settings', { settings });

/** Aggregator metadata exposed by `last_sync_info` so a freshly-opened
 *  window can hydrate its "Synced X ago" footer without waiting for the
 *  next backend tick. */
export interface LastSyncInfo {
  /** RFC 3339 timestamp of the most recent successful aggregator tick, or
   *  `null` before the first tick completes. */
  synced_at: string | null;
  /** Non-fatal error surfaced by the last tick (e.g. local-scan failure).
   *  Per-provider failures are logged backend-side and not propagated here. */
  last_error: string | null;
}

/** Read the aggregator's last-sync metadata. The cache itself is read via
 *  the existing `listWaiting` / `listRepos` / etc. commands. */
export const lastSyncInfo = (): Promise<LastSyncInfo> => invoke('last_sync_info');

/** Request an immediate aggregator tick. Returns as soon as the trigger is
 *  queued — the actual fetch happens in the backend polling task and
 *  surfaces via the `data-updated` event. Wired to the popover and main
 *  window refresh buttons. */
export const aggregatorRefreshNow = (): Promise<void> =>
  invoke('aggregator_refresh_now');

/** Payload emitted by the backend aggregator after each successful tick. */
export interface DataUpdatedPayload {
  synced_at: string;
}

/** Spawn the configured editor command with `path` appended. Fails if no
 *  editor_command is set in Settings. */
export const runEditor = (path: string): Promise<void> =>
  invoke('run_editor', { path });

/** Clone a remote repo to `<parentDir>/<folderName>`. When `accountId` is
 *  given the backend uses that account's stored token for HTTPS auth —
 *  required for private repos. Returns the absolute path of the new
 *  working directory. */
export const cloneRepo = (
  url: string,
  parentDir: string,
  folderName: string,
  accountId: string | null,
): Promise<string> =>
  invoke('clone_repo', { url, parentDir, folderName, accountId });

/** Build a (host, owner, name) → LocalRepo[] map for fast remote→local joins. */
export function indexLocalByRemote(locals: LocalRepo[]): Map<string, LocalRepo[]> {
  const map = new Map<string, LocalRepo[]>();
  for (const l of locals) {
    if (!l.remote || !l.remote.host) continue;
    const key = `${l.remote.host}:${l.remote.owner}/${l.remote.name}`.toLowerCase();
    const list = map.get(key);
    if (list) list.push(l);
    else map.set(key, [l]);
  }
  return map;
}

/** Build the local-index join key for a remote Repo. The host has to come
 *  from html_url because for self-hosted GitLab/Gitea instances we can't
 *  derive it from the Provider tag alone (different users connect to
 *  different hosts: gitlab.gwdg.de, gitlab.mpsd.mpg.de, …). */
export function localKeyForRepo(r: Repo): string {
  let host = '';
  try {
    host = new URL(r.html_url).host;
  } catch {
    host = providerHost[r.provider];
  }
  return `${host}:${r.owner}/${r.name}`.toLowerCase();
}
