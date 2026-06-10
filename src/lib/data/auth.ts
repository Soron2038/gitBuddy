// Derivation of the single-account-per-provider "heads" (viewer/gl/cb) from
// the canonical multi-account registry. Both windows render legacy UI that
// assumes at most one account per provider type; this picks the first match
// per provider out of `accountsList()` results. Centralised so the two
// windows can never disagree on what "connected" means (the popover used to
// read a different, legacy status path).

import type { Account, Viewer, GitLabStatus, CodebergStatus } from './api';

export interface ProviderHeads {
  viewer: Viewer | null;
  gl: GitLabStatus | null;
  cb: CodebergStatus | null;
}

export function deriveProviderHeads(accounts: Account[]): ProviderHeads {
  const gh = accounts.find((a) => a.provider === 'github');
  const glAcct = accounts.find(
    (a) => a.provider === 'gitlab' || a.provider === 'mpsd-gitlab',
  );
  const cbAcct = accounts.find((a) => a.provider === 'codeberg');
  return {
    viewer: gh?.viewer ?? null,
    gl:
      glAcct && glAcct.base_url
        ? { viewer: glAcct.viewer, base_url: glAcct.base_url }
        : null,
    cb:
      cbAcct && cbAcct.base_url
        ? { viewer: cbAcct.viewer, base_url: cbAcct.base_url }
        : null,
  };
}
