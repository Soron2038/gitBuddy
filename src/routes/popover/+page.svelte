<script lang="ts">
  import Buddy from '$lib/Buddy.svelte';
  import { waiting, stats, type Provider } from '$lib/data/stub';

  let activeTab: 'waiting' | 'repos' | 'releases' = $state('waiting');

  const providerLabel: Record<Provider, string> = {
    github: 'GitHub',
    gitlab: 'GitLab',
    codeberg: 'Codeberg',
    'mpsd-gitlab': 'MPSD',
  };
</script>

<div class="stage">
<div class="pop">
  <header class="pop-head">
    <Buddy size={28} />
    <span class="brand">git<em>Buddy</em></span>
    <span class="spc"></span>
    <button class="ib" title="Refresh" aria-label="Refresh">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
        <path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 4v5h-5" />
      </svg>
    </button>
    <button class="ib" title="Open main window" aria-label="Open main window">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M15 3h6v6" /><path d="M10 14 21 3" />
        <path d="M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5" />
      </svg>
    </button>
    <button class="ib" title="Settings" aria-label="Settings">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
      </svg>
    </button>
  </header>

  <p class="greeting">
    Hey <em>Björn</em> — {stats.waiting} things and {stats.newReleases} fresh releases.
  </p>

  <div class="tabs" role="tablist">
    <button
      class="tab"
      class:on={activeTab === 'waiting'}
      role="tab"
      aria-selected={activeTab === 'waiting'}
      onclick={() => (activeTab = 'waiting')}
    >
      Waiting <span class="n">{stats.waiting}</span>
    </button>
    <button
      class="tab"
      class:on={activeTab === 'repos'}
      role="tab"
      aria-selected={activeTab === 'repos'}
      onclick={() => (activeTab = 'repos')}
    >
      Repos <span class="n">147</span>
    </button>
    <button
      class="tab"
      class:on={activeTab === 'releases'}
      role="tab"
      aria-selected={activeTab === 'releases'}
      onclick={() => (activeTab = 'releases')}
    >
      Releases <span class="n">{stats.newReleases}</span>
    </button>
  </div>

  <div class="list" role="tabpanel">
    {#if activeTab === 'waiting'}
      {#each waiting as item (item.id)}
        <button class="row" type="button">
          <span class="chip {item.kind.toLowerCase()}">{item.kind}</span>
          <span class="body">
            <span class="title">{item.title}</span>
            <span class="meta">
              {item.repo} <span class="dot">·</span>
              <span class="reason">{item.reason}</span>
              <span class="prov-tag">{providerLabel[item.provider]}</span>
            </span>
          </span>
          <span class="age">{item.ageHuman}</span>
        </button>
      {/each}
    {:else}
      <div class="empty">
        <Buddy size={48} />
        <p>Nothing here yet.</p>
        <small>Coming in M2 — provider integrations.</small>
      </div>
    {/if}
  </div>

  <footer class="pop-foot">
    <span class="pulse" aria-hidden="true"></span>
    Synced 24 sec ago · next in 4m 36s
    <span class="spc"></span>
    <span class="kbd">⌘⇧G</span>
  </footer>
</div>
</div>

<style>
  /* The stage gives the .pop panel a fully-transparent margin so its drop
     shadow has room to fall *outside* the panel's rounded shape. Without
     this margin the shadow extends past the rounding into the corner gaps
     of the Tauri window and makes the corners look square. */
  .stage {
    width: 100vw;
    height: 100vh;
    padding: 20px;
    box-sizing: border-box;
    background: transparent;
  }
  /* Panel itself — fills the stage area (window minus 20px transparent margin).
     Shadow is tuned to stay well within that 20px margin so it can fade
     organically instead of being clipped by the window's bounding rectangle.
     Layering:
       1. Hairline 0.5px edge for crisp panel definition.
       2. Soft asymmetric drop — more below than above, the way a real shadow
          under a floating object behaves. */
  .pop {
    width: 100%;
    height: 100%;
    background: var(--paper);
    border-radius: var(--r-xl);
    box-shadow:
      0 0 0 0.5px rgba(46, 33, 27, 0.10),
      0 6px 14px -4px rgba(60, 40, 20, 0.18);
    overflow: hidden;
    display: flex;
    flex-direction: column;
  }

  /* Header --------------------------------------------------------- */
  .pop-head {
    padding: 16px 18px 10px;
    display: flex;
    align-items: center;
    gap: 10px;
    border-bottom: 1px solid var(--line);
    background: linear-gradient(180deg, #FFF9EC 0%, #FDF7EA 100%);
  }
  .brand {
    font-family: var(--font-display);
    font-size: 22px;
    letter-spacing: -0.02em;
    color: var(--ink);
  }
  .brand em {
    font-style: italic;
    color: var(--terracotta);
  }
  .spc { flex: 1; }
  .ib {
    width: 28px; height: 28px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    color: var(--ink-2);
  }
  .ib:hover { background: var(--cream-2); }

  /* Greeting ------------------------------------------------------- */
  .greeting {
    padding: 14px 18px 0;
    font-size: 13px;
    color: var(--ink-2);
    margin: 0;
  }
  .greeting em {
    font-family: var(--font-display);
    font-style: italic;
    color: var(--terracotta);
    font-weight: 400;
    font-size: 14px;
  }

  /* Tabs ----------------------------------------------------------- */
  .tabs {
    margin: 12px 18px 0;
    display: flex;
    gap: 4px;
    padding: 4px;
    background: var(--cream-2);
    border-radius: var(--r-md);
    font-size: 12.5px;
  }
  .tab {
    flex: 1;
    padding: 7px 8px;
    color: var(--ink-2);
    border-radius: 9px;
    text-align: center;
  }
  .tab.on {
    background: var(--paper);
    color: var(--ink);
    font-weight: 600;
    box-shadow: var(--shadow-1);
  }
  .tab .n {
    margin-left: 5px;
    font-family: var(--font-mono);
    font-size: 10.5px;
    color: var(--ink-3);
  }
  .tab.on .n { color: var(--terracotta); }

  /* List ----------------------------------------------------------- */
  .list {
    flex: 1;
    overflow-y: auto;
    overflow-x: hidden;
    padding: 8px 10px 10px;
    margin: 0;
    list-style: none;
  }
  .row {
    width: 100%;
    display: grid;
    grid-template-columns: 32px 1fr auto;
    gap: 10px;
    padding: 11px 10px;
    border-radius: var(--r-md);
    align-items: start;
    cursor: pointer;
    text-align: left;
  }
  .row:hover { background: var(--cream-2); }
  .body {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .title { display: block; }
  .chip {
    width: 26px; height: 26px;
    border-radius: var(--r-sm);
    display: grid; place-items: center;
    font-family: var(--font-mono);
    font-size: 9.5px;
    font-weight: 600;
    margin-top: 1px;
    letter-spacing: 0.04em;
  }
  .chip.pr { background: var(--sage-soft); color: #4A6048; }
  .chip.is { background: var(--terracotta-soft); color: #A0431F; }
  .chip.mr { background: var(--butter-soft); color: #9A6E1A; }
  .title {
    font-size: 13.5px;
    line-height: 1.3;
    color: var(--ink);
    font-weight: 500;
    letter-spacing: -0.005em;
  }
  .meta {
    margin-top: 4px;
    font-size: 11px;
    color: var(--ink-3);
    font-family: var(--font-mono);
    letter-spacing: 0.02em;
    display: flex;
    gap: 7px;
    align-items: center;
    flex-wrap: wrap;
  }
  .meta .dot { opacity: 0.5; }
  .meta .reason {
    color: var(--terracotta);
    font-weight: 500;
    text-transform: lowercase;
  }
  .meta .prov-tag {
    margin-left: auto;
    color: var(--ink-3);
    opacity: 0.7;
    font-size: 10px;
  }
  .age {
    font-family: var(--font-display);
    font-style: italic;
    font-size: 12px;
    color: var(--ink-3);
    margin-top: 2px;
  }

  .empty {
    list-style: none;
    text-align: center;
    padding: 50px 20px;
    color: var(--ink-3);
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 10px;
  }
  .empty p {
    margin: 0;
    font-family: var(--font-display);
    font-size: 18px;
    color: var(--ink-2);
  }
  .empty small {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-4);
  }

  /* Footer --------------------------------------------------------- */
  .pop-foot {
    padding: 11px 16px;
    border-top: 1px solid var(--line);
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--ink-3);
    background: var(--paper-2);
    font-family: var(--font-mono);
  }
  .pulse {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sage);
    box-shadow: 0 0 0 0 rgba(128, 152, 123, 0.5);
    animation: pulse 2.4s ease-out infinite;
  }
  @keyframes pulse {
    0%   { box-shadow: 0 0 0 0 rgba(128, 152, 123, 0.5); }
    70%  { box-shadow: 0 0 0 7px rgba(128, 152, 123, 0); }
    100% { box-shadow: 0 0 0 0 rgba(128, 152, 123, 0); }
  }
  .kbd {
    background: var(--cream-2);
    padding: 2px 6px;
    border-radius: 5px;
    color: var(--ink-2);
  }
</style>
