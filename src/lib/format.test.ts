import { describe, it, expect } from 'vitest';
import {
  humaniseSync,
  repoAge,
  shortenPath,
  connectedHosts,
  hostSuggestions,
  repoKey,
  releaseKey,
  dedupeBy,
} from './format';
import type { LocalRepo } from './data/api';

/** Minimal LocalRepo with just the remote host the suggestion logic reads. */
function local(host: string | null): LocalRepo {
  return {
    path: `/tmp/${host ?? 'none'}`,
    branch: 'main',
    remote: host ? { host, owner: 'o', name: 'r', raw_url: `https://${host}/o/r` } : null,
    dirty_staged: 0,
    dirty_unstaged: 0,
    untracked: 0,
    ahead: 0,
    behind: 0,
    detached: false,
  };
}

describe('repoKey / releaseKey', () => {
  // C4: the aggregator emits one row per (account, repo) pair by design, so
  // two accounts seeing the same repo produce two rows carrying the *same*
  // forge id. Keying a {#each} on that id threw each_key_duplicate and blanked
  // the whole list — in production builds too, not just dev.
  it('separates two instances that share a forge-side id', () => {
    const a = { id: 'gl:7', html_url: 'https://gitlab.gwdg.de/g/p' };
    const b = { id: 'gl:7', html_url: 'https://gitlab.mpsd.mpg.de/g/p' };
    expect(repoKey(a)).not.toBe(repoKey(b));
  });

  it('collapses the same repo surfaced by two accounts', () => {
    const viaPersonal = { id: '42', html_url: 'https://github.com/acme/api' };
    const viaWork = { id: '42', html_url: 'https://github.com/acme/api' };
    expect(repoKey(viaPersonal)).toBe(repoKey(viaWork));
  });

  it('falls back to the id when html_url is empty', () => {
    expect(repoKey({ id: 'cb:codeberg.org:3', html_url: '' })).toBe('cb:codeberg.org:3');
  });

  it('keys releases by url, falling back to repo + tag', () => {
    expect(releaseKey({ html_url: 'https://x/v1', repo_id: '1', tag: 'v1' })).toBe('https://x/v1');
    expect(releaseKey({ html_url: '', repo_id: '1', tag: 'v1' })).toBe('1:v1');
  });
});

describe('dedupeBy', () => {
  it('keeps the first occurrence and preserves order', () => {
    const items = [
      { id: 'a', n: 1 },
      { id: 'b', n: 2 },
      { id: 'a', n: 3 },
      { id: 'c', n: 4 },
    ];
    expect(dedupeBy(items, (i) => i.id).map((i) => i.n)).toEqual([1, 2, 4]);
  });

  it('leaves an already-unique list untouched', () => {
    const items = [{ id: 'a' }, { id: 'b' }];
    expect(dedupeBy(items, (i) => i.id)).toHaveLength(2);
  });

  it('handles an empty list', () => {
    expect(dedupeBy([], () => '')).toEqual([]);
  });
});

describe('hostSuggestions', () => {
  // N14: the GitLab filter used to carry an `out.size > 0` escape hatch, which
  // let whichever host was scanned *first* through unconditionally. So a
  // bitbucket.org clone was offered as a GitLab instance, and reordering the
  // scan silently changed the suggestions.
  it('never offers a non-gitlab host for gitlab, whatever the order', () => {
    const hosts = [local('bitbucket.org'), local('gitlab.gwdg.de')];
    expect(hostSuggestions('gitlab', hosts, new Set())).toEqual(['gitlab.gwdg.de']);
    expect(hostSuggestions('gitlab', [...hosts].reverse(), new Set())).toEqual([
      'gitlab.gwdg.de',
    ]);
  });

  it('suggests nothing rather than guessing when no host looks gitlab-y', () => {
    expect(hostSuggestions('gitlab', [local('bitbucket.org')], new Set())).toEqual([]);
  });

  it('offers non-gitlab hosts for codeberg and excludes gitlab-y ones', () => {
    const hosts = [local('codeberg.org'), local('gitlab.gwdg.de'), local('git.example.com')];
    expect(hostSuggestions('codeberg', hosts, new Set())).toEqual([
      'codeberg.org',
      'git.example.com',
    ]);
  });

  it('skips github.com, already-connected hosts and remoteless clones', () => {
    const hosts = [local('github.com'), local('gitlab.a.de'), local('gitlab.b.de'), local(null)];
    expect(hostSuggestions('gitlab', hosts, new Set(['gitlab.a.de']))).toEqual(['gitlab.b.de']);
  });

  it('deduplicates and sorts', () => {
    const hosts = [local('gitlab.b.de'), local('gitlab.a.de'), local('gitlab.b.de')];
    expect(hostSuggestions('gitlab', hosts, new Set())).toEqual(['gitlab.a.de', 'gitlab.b.de']);
  });
});

describe('connectedHosts', () => {
  it('parses hosts and skips nullish or malformed entries', () => {
    const set = connectedHosts(['https://gitlab.gwdg.de', null, undefined, 'not a url', '']);
    expect([...set]).toEqual(['gitlab.gwdg.de']);
  });

  // A substring check would let a stored `https://gitlab.com` match a local
  // host like `lab.com` — the bug the popover's old check had.
  it('compares whole hosts, not substrings', () => {
    const set = connectedHosts(['https://gitlab.com']);
    expect(set.has('lab.com')).toBe(false);
    expect(set.has('gitlab.com')).toBe(true);
  });
});

describe('humaniseSync', () => {
  const now = Date.UTC(2026, 6, 28, 12, 0, 0);
  const ago = (seconds: number) => new Date(now - seconds * 1000);

  it('reports never before the first sync', () => {
    expect(humaniseSync(null, now)).toBe('never');
  });

  it('walks the buckets', () => {
    expect(humaniseSync(ago(2), now)).toBe('just now');
    expect(humaniseSync(ago(30), now)).toBe('30 sec ago');
    expect(humaniseSync(ago(90), now)).toBe('1 min ago');
    expect(humaniseSync(ago(3600), now)).toBe('1h ago');
  });

  it('clamps a clock that ran backwards instead of showing a negative age', () => {
    expect(humaniseSync(new Date(now + 60_000), now)).toBe('just now');
  });
});

describe('repoAge', () => {
  const now = Date.UTC(2026, 6, 28, 12, 0, 0);
  const minutesAgo = (m: number) => new Date(now - m * 60_000).toISOString();

  it('returns an em dash when the push time is unknown', () => {
    expect(repoAge(null, now)).toBe('—');
  });

  it('never reports 0m for a very recent push', () => {
    expect(repoAge(minutesAgo(0), now)).toBe('1m');
  });

  it('walks the buckets', () => {
    expect(repoAge(minutesAgo(30), now)).toBe('30m');
    expect(repoAge(minutesAgo(60 * 5), now)).toBe('5h');
    expect(repoAge(minutesAgo(60 * 24 * 3), now)).toBe('3d');
    expect(repoAge(minutesAgo(60 * 24 * 60), now)).toBe('2mo');
    expect(repoAge(minutesAgo(60 * 24 * 400), now)).toBe('1y');
  });
});

describe('shortenPath', () => {
  it('keeps short paths whole and trims long ones to the last two parts', () => {
    expect(shortenPath('/Users/witt')).toBe('/Users/witt');
    expect(shortenPath('/Users/witt/Developer/gitBuddy')).toBe('…/Developer/gitBuddy');
  });
});
