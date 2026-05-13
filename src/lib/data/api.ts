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

export interface Settings {
  scan_roots: string[];
  scan_ignore: string[];
  gitlab_base_url: string | null;
  codeberg_base_url: string | null;
  /** Shell command spawned by "Open in editor" — repo path is appended.
   *  Empty/null disables that quick-action menu entry. */
  editor_command: string | null;
  /** When true, the popover fires a native notification whenever a poll
   *  surfaces a waiting item that wasn't there on the previous refresh. */
  notifications_enabled: boolean;
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
}

export type CiStatus = 'ok' | 'fail' | 'run' | 'cancelled' | 'none';

export interface CiRun {
  repo_id: string;
  repo_full_name: string;
  status: CiStatus;
  html_url: string | null;
  branch: string | null;
  workflow_name: string | null;
}

// ── Tauri commands ─────────────────────────────────────────────────────────

// ── Per-provider auth ──────────────────────────────────────────────────────

export const ghStatus = (): Promise<Viewer | null> => invoke('gh_status');
export const ghSetToken = (token: string): Promise<Viewer> =>
  invoke('gh_set_token', { token });

export const glStatus = (): Promise<GitLabStatus | null> => invoke('gl_status');
export const glSetToken = (token: string, baseUrl: string): Promise<Viewer> =>
  invoke('gl_set_token', { token, baseUrl });

export const cbStatus = (): Promise<CodebergStatus | null> => invoke('cb_status');
export const cbSetToken = (token: string, baseUrl: string): Promise<Viewer> =>
  invoke('cb_set_token', { token, baseUrl });

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

/** Spawn the configured editor command with `path` appended. Fails if no
 *  editor_command is set in Settings. */
export const runEditor = (path: string): Promise<void> =>
  invoke('run_editor', { path });

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
