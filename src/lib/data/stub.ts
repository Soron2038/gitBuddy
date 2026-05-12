// Stub data for M1. Replaced by real provider integrations in M2+.
// Keeping the shape close to the eventual Rust-side data model so the swap
// is mechanical.

export type Provider = 'github' | 'gitlab' | 'codeberg' | 'mpsd-gitlab';

export type ItemKind = 'PR' | 'IS' | 'MR';
export type ItemReason = 'assigned' | 'review' | 'authored' | 'mentioned';

export type CiStatus = 'ok' | 'fail' | 'run' | 'none';

export interface WaitingItem {
  id: string;
  kind: ItemKind;
  title: string;
  repo: string;        // "org/name"
  provider: Provider;
  reason: ItemReason;
  ageHuman: string;    // "2h", "1d", … (server-rendered later)
}

export interface Repo {
  id: string;
  owner: string;
  name: string;
  provider: Provider;
  branch: string;
  language: string;
  version?: string;
  versionIsNew?: boolean;
  ci: CiStatus;
  localPath?: string;
  issues: number;
  prs: number;
  hotCount?: number;   // counters that should be highlighted
  warnings?: string[]; // e.g. "3 unpushed", "7 uncommitted"
}

export const waiting: WaitingItem[] = [
  { id: '1', kind: 'PR', title: 'Fix: prevent race condition in scheduler',  repo: 'anthropics/claude-code', provider: 'github',       reason: 'review',    ageHuman: '2h' },
  { id: '2', kind: 'IS', title: 'Onboarding flow crashes on Safari 17',      repo: 'vercel/next.js',          provider: 'github',       reason: 'assigned',  ageHuman: '1d' },
  { id: '3', kind: 'MR', title: 'Add support for nested groups',             repo: 'gitlab-org/gitlab',       provider: 'gitlab',       reason: 'authored',  ageHuman: '3d' },
  { id: '4', kind: 'IS', title: 'Memory leak in worker pool',                repo: 'mpsd/api-gateway',        provider: 'mpsd-gitlab',  reason: 'mentioned', ageHuman: '5h' },
  { id: '5', kind: 'PR', title: 'Refactor token storage to keyring',         repo: 'forgejo/runner',          provider: 'codeberg',     reason: 'review',    ageHuman: '6h' },
  { id: '6', kind: 'IS', title: 'Document new auth scope requirements',      repo: 'witt/gitBuddy',           provider: 'github',       reason: 'assigned',  ageHuman: '2d' },
];

export const repos: Repo[] = [
  {
    id: 'r1', owner: 'anthropics', name: 'claude-code', provider: 'github',
    branch: 'main', language: 'TypeScript',
    version: 'v1.0.93', versionIsNew: true, ci: 'ok',
    localPath: '~/Developer/claude-code',
    issues: 4, prs: 2, hotCount: 4,
  },
  {
    id: 'r2', owner: 'vercel', name: 'next.js', provider: 'github',
    branch: 'canary', language: 'TypeScript',
    version: 'v15.0.2', ci: 'ok',
    issues: 17, prs: 3,
  },
  {
    id: 'r3', owner: 'gitlab-org', name: 'gitlab', provider: 'gitlab',
    branch: 'master', language: 'Ruby',
    version: '17.5.0', ci: 'fail',
    issues: 8, prs: 1,
  },
  {
    id: 'r4', owner: 'forgejo', name: 'runner', provider: 'codeberg',
    branch: 'main', language: 'Go',
    version: 'v6.1.0', versionIsNew: true, ci: 'ok',
    localPath: '~/Developer/forgejo-runner',
    issues: 3, prs: 0,
  },
  {
    id: 'r5', owner: 'mpsd', name: 'api-gateway', provider: 'mpsd-gitlab',
    branch: 'develop', language: 'Rust',
    version: '0.18.2', ci: 'run',
    localPath: '~/Developer/work/api-gw',
    issues: 2, prs: 1, hotCount: 2,
    warnings: ['3 unpushed'],
  },
  {
    id: 'r6', owner: 'witt', name: 'dotfiles', provider: 'github',
    branch: 'main', language: 'Shell',
    ci: 'none',
    localPath: '~/dotfiles',
    issues: 0, prs: 0,
    warnings: ['7 uncommitted'],
  },
];

export const stats = {
  waiting: 12,
  ciPassing: 134,
  ciTotal: 141,
  newReleases: 3,
  localClones: 31,
  withUncommitted: 9,
};
