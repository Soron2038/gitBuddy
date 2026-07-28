// Shared display formatters used by both the main window and the popover.
// Before this module existed, each route carried its own copy — with subtle
// drift (a substring host check that false-matched, two different "no sync
// yet" placeholders). Centralised here so both surfaces format identically.

import type { LocalRepo, Release, Repo } from '$lib/data/api';

/** Relative "synced X ago" label. `nowMs` is passed in (rather than read via
 *  `Date.now()`) so a once-per-second `$state` tick re-derives the text while
 *  no fetch is happening. Returns "never" when no sync has completed yet. */
export function humaniseSync(d: Date | null, nowMs: number): string {
  if (!d) return 'never';
  const s = Math.max(0, Math.floor((nowMs - d.getTime()) / 1000));
  if (s < 5) return 'just now';
  if (s < 60) return `${s} sec ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  return `${Math.floor(m / 60)}h ago`;
}

/** Compact age of a repo's last push ("3d", "2mo", "1y"); "—" when unknown.
 *  `nowMs` is passed in (same contract as {@link humaniseSync}) so the
 *  caller's once-per-second tick keeps displayed ages fresh — an internal
 *  `Date.now()` would be invisible to Svelte's reactivity and go stale. */
export function repoAge(pushed_at: string | null, nowMs: number): string {
  if (!pushed_at) return '—';
  const d = new Date(pushed_at);
  const mins = Math.floor((nowMs - d.getTime()) / 60_000);
  if (mins < 60) return `${Math.max(1, mins)}m`;
  if (mins < 60 * 24) return `${Math.floor(mins / 60)}h`;
  if (mins < 60 * 24 * 30) return `${Math.floor(mins / (60 * 24))}d`;
  if (mins < 60 * 24 * 365) return `${Math.floor(mins / (60 * 24 * 30))}mo`;
  return `${Math.floor(mins / (60 * 24 * 365))}y`;
}

/** Shorten an absolute path for compact display: trim to the last two path
 *  components if it's longer. The Rust side returns absolute paths and we
 *  don't know $HOME on the JS side, so this is cosmetic only. */
export function shortenPath(p: string): string {
  const parts = p.split('/').filter(Boolean);
  if (parts.length <= 2) return p;
  return `…/${parts.slice(-2).join('/')}`;
}

/** Stable, collision-free key for a repo row.
 *
 *  The aggregator deliberately returns one row per *(account, repo)* pair —
 *  two accounts that can both see `acme/api` produce two rows carrying the
 *  same forge id. Keying a `{#each}` on `repo.id` therefore throws
 *  `each_key_duplicate`, which kills the render effect and blanks the whole
 *  list (Svelte 5 throws in production builds too, not just dev). `html_url`
 *  is the identity the dedup below already uses and is unique per repo across
 *  every forge and instance. */
export function repoKey(r: Pick<Repo, 'html_url' | 'id'>): string {
  return r.html_url || r.id;
}

/** Same contract as {@link repoKey}, for release rows: two accounts seeing the
 *  same repo both report its latest release. */
export function releaseKey(r: Pick<Release, 'html_url' | 'repo_id' | 'tag'>): string {
  return r.html_url || `${r.repo_id}:${r.tag}`;
}

/** Collapse duplicate rows, keeping the first occurrence of each key.
 *  Used for the lists that are rendered as-is (the popover's repo and release
 *  tabs); the main window's repo grid runs its own dedup because it also
 *  merges the per-account badges. */
export function dedupeBy<T>(items: T[], key: (item: T) => string): T[] {
  const seen = new Set<string>();
  const out: T[] = [];
  for (const item of items) {
    const k = key(item);
    if (seen.has(k)) continue;
    seen.add(k);
    out.push(item);
  }
  return out;
}

/** Build the set of already-connected hosts from a list of account base URLs.
 *  Parses each via the URL API (a real host comparison) so a stored
 *  `https://gitlab.com` can't false-match a local host like `lab.com` — the
 *  bug the popover's old `String.includes` check had. Malformed URLs and
 *  nullish entries (e.g. GitHub's null base_url) are skipped. */
export function connectedHosts(baseUrls: Array<string | null | undefined>): Set<string> {
  const set = new Set<string>();
  for (const url of baseUrls) {
    if (!url) continue;
    try {
      set.add(new URL(url).host);
    } catch {
      /* malformed base_url — skip */
    }
  }
  return set;
}

/** Suggest hosts to pre-fill the GitLab/Codeberg connect form, derived from
 *  the user's local clones. Skips github.com, anything already connected, and
 *  applies a cheap "contains gitlab" heuristic to bucket hosts by target.
 *  `connected` is the output of {@link connectedHosts}. */
export function hostSuggestions(
  target: 'gitlab' | 'codeberg',
  locals: LocalRepo[],
  connected: Set<string>,
): string[] {
  const out = new Set<string>();
  for (const o of locals) {
    const h = o.remote?.host;
    if (!h) continue;
    if (h === 'github.com') continue;
    if (connected.has(h)) continue;
    // Cheap heuristic: hosts containing "gitlab" are GitLab-y, anything else
    // is offered for Codeberg/Gitea. We don't gatekeep too strictly — the
    // user might know better.
    const isGitlabLike = h.includes('gitlab');
    if (target === 'gitlab' && !isGitlabLike) continue;
    if (target === 'codeberg' && isGitlabLike) continue;
    out.add(h);
  }
  // The GitLab filter used to carry an `out.size > 0` escape hatch, which let
  // whichever host happened to be scanned *first* through unconditionally —
  // so a `bitbucket.org` clone would be offered as a GitLab instance, and
  // reordering the scan changed the suggestions. If nothing looks GitLab-y we
  // now say so by suggesting nothing, rather than guessing wrong.
  return Array.from(out).sort();
}
