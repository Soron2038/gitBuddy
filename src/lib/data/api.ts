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

export const providerLabel: Record<Provider, string> = {
  github: 'GitHub',
  gitlab: 'GitLab',
  codeberg: 'Codeberg',
  'mpsd-gitlab': 'MPSD',
};

/** Host portion the local indexer would record on `origin` for each provider.
 *  Used to join LocalRepo.remote → Repo. */
export const providerHost: Record<Provider, string> = {
  github: 'github.com',
  gitlab: 'gitlab.com',
  codeberg: 'codeberg.org',
  'mpsd-gitlab': 'gitlab.mpsd.mpg.de',
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
}

// ── Tauri commands ─────────────────────────────────────────────────────────

/** Returns the currently-connected GitHub viewer, or null if no account is set. */
export const ghStatus = (): Promise<Viewer | null> => invoke('gh_status');

/** Verifies a GitHub PAT, stores it in the Keychain, and activates it. */
export const ghSetToken = (token: string): Promise<Viewer> =>
  invoke('gh_set_token', { token });

/** Items where the user is assigned, review-requested, authored, or mentioned. */
export const ghListWaiting = (): Promise<WaitingItem[]> => invoke('gh_list_waiting');

/** All repos the viewer can see — owned, collaborator, or org member. */
export const ghListRepos = (): Promise<Repo[]> => invoke('gh_list_repos');

/** Scan configured roots and report every local checkout with diagnostics. */
export const listLocalRepos = (): Promise<LocalRepo[]> => invoke('list_local_repos');

/** Load persisted user settings (scan roots, ignore patterns). */
export const getSettings = (): Promise<Settings> => invoke('get_settings');

/** Persist user settings to the OS config directory. */
export const saveSettings = (settings: Settings): Promise<void> =>
  invoke('save_settings', { settings });

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

export function localKeyForRepo(r: Repo): string {
  return `${providerHost[r.provider]}:${r.owner}/${r.name}`.toLowerCase();
}
