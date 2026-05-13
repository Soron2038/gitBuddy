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
