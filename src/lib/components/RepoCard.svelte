<script module lang="ts">
  import type { Repo } from '$lib/data/api';

  /** A Repo enriched with every account that surfaced it — the page dedups
   *  the aggregator's one-row-per-(account, repo) output into one entry per
   *  unique html_url, with a badge per origin account. */
  export type RepoEntry = Repo & { account_ids: string[] };
</script>

<script lang="ts">
  // One repo card in the main window's grid (the "All repos" and "Local
  // clones" views). Extracted from routes/+page.svelte's repoCardEntry
  // snippet. Shared chip styles (.pchip, .rci) come from
  // routes/main-window.css; everything card-specific is scoped here.
  import {
    providerChipText,
    providerCssClass,
    type Account,
    type CiStatus,
    type LocalRepo,
  } from '$lib/data/api';

  interface Props {
    /** The (deduped) repo this card renders. */
    entry: RepoEntry;
    /** Local clones joined onto this repo; undefined = not cloned. */
    local: LocalRepo[] | undefined;
    ci: CiStatus | 'none';
    selected: boolean;
    /** Render one chip per contributing account instead of the provider
     *  chip — only meaningful with more than one connected account. */
    showAccountBadges: boolean;
    accountById: Map<string, Account>;
    onselect: () => void;
    oncontextmenu: (e: MouseEvent) => void;
  }

  let {
    entry: r,
    local,
    ci,
    selected,
    showAccountBadges,
    accountById,
    onselect,
    oncontextmenu,
  }: Props = $props();

  let firstLocal = $derived(local?.[0]);
</script>

<button
  class="card"
  class:selected={selected}
  onclick={onselect}
  {oncontextmenu}
>
  {#if showAccountBadges && r.account_ids.length > 0}
    <span class="acct-badges">
      {#each r.account_ids as id (id)}
        {@const a = accountById.get(id)}
        {#if a}
          {@const aHost = a.base_url
            ? (() => {
                try {
                  return new URL(a.base_url!).host;
                } catch {
                  return a.base_url!;
                }
              })()
            : 'github.com'}
          <span
            class="pchip {providerCssClass(a.provider)}"
            title="{a.login}@{aHost}"
          >
            {providerChipText({ provider: a.provider, html_url: a.base_url ?? '' })}
          </span>
        {/if}
      {/each}
    </span>
  {:else}
    <span class="pchip {providerCssClass(r.provider)}">{providerChipText(r)}</span>
  {/if}
  <div class="rname">
    <span class="owner">{r.owner}</span> / <b>{r.name}</b>
    <div class="sub">
      {#if local}
        <span class="pin">
          <span
            class="d"
            class:off={firstLocal && (firstLocal.dirty_staged + firstLocal.dirty_unstaged + firstLocal.untracked > 0 || firstLocal.ahead > 0)}
          ></span>
          {firstLocal?.path ?? 'cloned'}
        </span>
      {:else}
        <span class="pin">
          <span class="d off"></span> not cloned
        </span>
      {/if}
      <span>{r.default_branch}</span>
      {#if r.is_private}<span>private</span>{/if}
      {#if r.is_fork}<span>fork</span>{/if}
      {#if firstLocal && (firstLocal.dirty_staged + firstLocal.dirty_unstaged > 0)}
        <span class="warn">{firstLocal.dirty_staged + firstLocal.dirty_unstaged} uncommitted</span>
      {/if}
      {#if firstLocal && firstLocal.ahead > 0}
        <span class="warn">{firstLocal.ahead} unpushed</span>
      {/if}
    </div>
  </div>
  <div class="rmeta">
    <span class="rci {ci}">
      <span class="b"></span>
      {#if ci === 'ok'}passing{:else if ci === 'fail'}failing{:else if ci === 'run'}running{:else if ci === 'cancelled'}cancelled{:else}no ci{/if}
    </span>
    {#if r.language}
      <span class="lang">{r.language}</span>
    {/if}
    {#if r.stars > 0}
      <span class="stars">★ {r.stars}</span>
    {/if}
  </div>
</button>

<style>
.card {
  width: 100%;
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-lg);
  padding: 14px 16px;
  display: grid;
  grid-template-columns: 32px 1fr auto;
  gap: 12px;
  align-items: start;
  text-align: left;
  transition: transform 0.15s, box-shadow 0.15s;
  cursor: pointer;
}
.card:hover {
  transform: translateY(-1px);
  box-shadow: var(--shadow-2);
}

/* Stack of provider/host chips on a repo card — appears in place of the
   single .pchip when more than one account is connected, so the user can
   see which of their accounts surface this repo. */
.acct-badges {
  display: inline-flex;
  flex-direction: column;
  align-items: flex-start;
  gap: 3px;
}

.rname { line-height: 1.25; min-width: 0; }
.rname .owner { color: var(--ink-3); font-weight: 400; font-size: 12.5px; }
.rname b { font-weight: 600; font-size: 14.5px; color: var(--ink); }
.rname .sub {
  display: flex;
  gap: 10px;
  flex-wrap: wrap;
  margin-top: 5px;
  font-size: 11.5px;
  color: var(--ink-3);
  font-family: var(--font-mono);
  letter-spacing: 0.01em;
}
.rname .sub .pin {
  display: inline-flex;
  gap: 4px;
  align-items: center;
  max-width: 200px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.rname .sub .pin .d {
  width: 5px;
  height: 5px;
  border-radius: 50%;
  background: var(--sage);
  flex-shrink: 0;
}
.rname .sub .pin .d.off {
  background: var(--ink-4);
  opacity: 0.5;
}
.rname .sub .warn { color: var(--terracotta); }
.rmeta {
  text-align: right;
  display: flex;
  flex-direction: column;
  align-items: flex-end;
  gap: 5px;
  font-size: 12px;
  color: var(--ink-2);
  white-space: nowrap;
}
.lang, .stars {
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--ink-3);
}

.card.selected {
  background: var(--terracotta-soft);
  border-color: rgba(198, 98, 67, 0.22);
  box-shadow: var(--shadow-2);
}
.card.selected:hover {
  transform: none;
}
</style>
