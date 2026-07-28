import { describe, it, expect } from 'vitest';
import { deriveProviderHeads } from './auth';
import type { Account, Provider } from './api';

function account(provider: Provider, login: string, base_url: string | null = null): Account {
  return {
    id: `${provider}:host:${login}`,
    provider,
    login,
    viewer: { login, avatar_url: null, name: null },
    auth: 'pat',
    base_url,
    added_at: '2026-07-28T00:00:00Z',
  };
}

describe('deriveProviderHeads', () => {
  it('reports nothing connected for an empty registry', () => {
    expect(deriveProviderHeads([])).toEqual({ viewer: null, gl: null, cb: null });
  });

  // N12: the popover gated its refresh button and its "Synced …" footer on
  // `viewer`, which is only the GitHub head — so a GitLab-only user read
  // "Not connected" above a full list of their own merge requests. Both
  // windows now derive `connected` from all three heads, which only works if
  // this returns the GitLab head independently of GitHub.
  it('surfaces a GitLab-only account without a GitHub one', () => {
    const heads = deriveProviderHeads([account('gitlab', 'bwitt', 'https://gitlab.gwdg.de')]);
    expect(heads.viewer).toBeNull();
    expect(heads.gl?.base_url).toBe('https://gitlab.gwdg.de');
    expect(heads.cb).toBeNull();
  });

  it('treats a self-hosted instance as the GitLab head', () => {
    const heads = deriveProviderHeads([
      account('mpsd-gitlab', 'bwitt', 'https://gitlab.mpsd.mpg.de'),
    ]);
    expect(heads.gl?.base_url).toBe('https://gitlab.mpsd.mpg.de');
  });

  it('picks the first account per provider and ignores the rest', () => {
    const heads = deriveProviderHeads([
      account('github', 'personal'),
      account('github', 'work'),
      account('gitlab', 'first', 'https://a.de'),
      account('gitlab', 'second', 'https://b.de'),
    ]);
    expect(heads.viewer?.login).toBe('personal');
    expect(heads.gl?.base_url).toBe('https://a.de');
  });

  // GitHub is the only provider without a base_url; for the self-hostable
  // ones a missing base_url means the record is unusable, so it must not be
  // reported as connected.
  it('drops a GitLab or Codeberg account that has no base URL', () => {
    const heads = deriveProviderHeads([
      account('gitlab', 'bwitt', null),
      account('codeberg', 'bwitt', null),
    ]);
    expect(heads.gl).toBeNull();
    expect(heads.cb).toBeNull();
  });

  it('resolves all three at once', () => {
    const heads = deriveProviderHeads([
      account('codeberg', 'cbuser', 'https://codeberg.org'),
      account('github', 'ghuser'),
      account('gitlab', 'gluser', 'https://gitlab.com'),
    ]);
    expect(heads.viewer?.login).toBe('ghuser');
    expect(heads.gl?.viewer.login).toBe('gluser');
    expect(heads.cb?.viewer.login).toBe('cbuser');
  });
});
