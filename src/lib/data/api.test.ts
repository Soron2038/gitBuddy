import { describe, it, expect } from 'vitest';
import {
  providerLabel,
  providerChipText,
  providerCssClass,
  localKeyForRepo,
  indexLocalByRemote,
  defaultSettings,
} from './api';
import type { LocalRepo, Repo } from './api';

function repo(over: Partial<Repo> = {}): Repo {
  return {
    id: '1',
    owner: 'acme',
    name: 'api',
    provider: 'github',
    default_branch: 'main',
    language: null,
    description: null,
    stars: 0,
    html_url: 'https://github.com/acme/api',
    ssh_url: null,
    clone_url: null,
    is_fork: false,
    is_private: false,
    pushed_at: null,
    account_id: null,
    ...over,
  };
}

function local(host: string, owner: string, name: string): LocalRepo {
  return {
    path: `/src/${name}`,
    branch: 'main',
    remote: { host, owner, name, raw_url: `https://${host}/${owner}/${name}` },
    dirty_staged: 0,
    dirty_unstaged: 0,
    untracked: 0,
    ahead: 0,
    behind: 0,
    detached: false,
  };
}

describe('provider chips', () => {
  // N9: `Account.provider` is always plain `gitlab` — the backend collapses
  // self-hosted instances when it writes the account record — while
  // `Repo.provider` distinguishes them. So the same repo showed a plum "gw"
  // chip when the badge came from the repo and an orange gitlab.com "gl" chip
  // when it came from the account, which is what happens as soon as a second
  // account switches the per-account badges on.
  it('resolves a self-hosted instance from the host, whichever tag it carries', () => {
    const fromRepo = { provider: 'mpsd-gitlab' as const, html_url: 'https://gitlab.gwdg.de/g/p' };
    const fromAccount = { provider: 'gitlab' as const, html_url: 'https://gitlab.gwdg.de' };
    expect(providerChipText(fromAccount)).toBe(providerChipText(fromRepo));
    expect(providerCssClass(fromAccount)).toBe(providerCssClass(fromRepo));
    expect(providerCssClass(fromAccount)).toBe('gl-self');
  });

  it('leaves gitlab.com itself alone', () => {
    const dotcom = { provider: 'gitlab' as const, html_url: 'https://gitlab.com/g/p' };
    expect(providerChipText(dotcom)).toBe('gl');
    expect(providerCssClass(dotcom)).toBe('gl');
    expect(providerLabel(dotcom)).toBe('GitLab');
  });

  it('treats a gitlab.com subdomain as gitlab.com, not as self-hosted', () => {
    const sub = { provider: 'gitlab' as const, html_url: 'https://foo.gitlab.com/g/p' };
    expect(providerCssClass(sub)).toBe('gl');
  });

  it('derives a two-letter slug from the instance host', () => {
    // The leading gitlab./git./code. subdomain is skipped so the slug names
    // the owner rather than the product.
    expect(providerChipText({ provider: 'mpsd-gitlab', html_url: 'https://gitlab.gwdg.de/x' })).toBe('gw');
    expect(providerChipText({ provider: 'mpsd-gitlab', html_url: 'https://gitlab.mpsd.mpg.de/x' })).toBe('mp');
    expect(providerChipText({ provider: 'mpsd-gitlab', html_url: 'https://git.example.com/x' })).toBe('ex');
  });

  it('names the host in the label for a self-hosted instance', () => {
    expect(providerLabel({ provider: 'mpsd-gitlab', html_url: 'https://gitlab.gwdg.de/x' })).toBe(
      'gitlab.gwdg.de',
    );
  });

  it('still accepts a bare provider string for the css class', () => {
    expect(providerCssClass('github')).toBe('gh');
    expect(providerCssClass('codeberg')).toBe('cb');
  });

  it('survives a missing or malformed url', () => {
    expect(providerChipText({ provider: 'gitlab', html_url: '' })).toBe('gl');
    expect(providerChipText({ provider: 'gitlab', html_url: 'nonsense' })).toBe('gl');
  });
});

describe('local-index join keys', () => {
  it('matches a remote repo to its clone case-insensitively', () => {
    const index = indexLocalByRemote([local('github.com', 'Acme', 'API')]);
    expect(index.get(localKeyForRepo(repo()))).toHaveLength(1);
  });

  it('keeps two instances apart even when owner and name match', () => {
    const index = indexLocalByRemote([
      local('gitlab.gwdg.de', 'g', 'p'),
      local('gitlab.mpsd.mpg.de', 'g', 'p'),
    ]);
    const gwdg = repo({ provider: 'mpsd-gitlab', html_url: 'https://gitlab.gwdg.de/g/p', owner: 'g', name: 'p' });
    expect(index.get(localKeyForRepo(gwdg))?.[0].path).toBe('/src/p');
    expect(index.size).toBe(2);
  });

  it('groups several clones of one repo under the same key', () => {
    const index = indexLocalByRemote([
      local('github.com', 'acme', 'api'),
      { ...local('github.com', 'acme', 'api'), path: '/other/api' },
    ]);
    expect(index.get(localKeyForRepo(repo()))).toHaveLength(2);
  });

  // N23: origin_remote used to return a RemoteRef of three empty strings when
  // the URL didn't parse, so every unparseable remote collided under
  // ("", "", ""). It returns null now, and these must simply not be indexed.
  it('ignores clones without a usable remote', () => {
    const orphan: LocalRepo = { ...local('github.com', 'a', 'b'), remote: null };
    expect(indexLocalByRemote([orphan]).size).toBe(0);
  });

  it('falls back to the provider host when html_url is unusable', () => {
    const broken = repo({ html_url: 'nonsense', provider: 'codeberg' });
    expect(localKeyForRepo(broken)).toBe('codeberg.org:acme/api');
  });
});

describe('defaultSettings', () => {
  it('starts with notifications on and the poll interval inside the backend band', () => {
    const s = defaultSettings();
    expect(s.notifications.enabled).toBe(true);
    expect(s.poll_interval_minutes).toBeGreaterThanOrEqual(1);
    expect(s.poll_interval_minutes).toBeLessThanOrEqual(60);
  });
});
