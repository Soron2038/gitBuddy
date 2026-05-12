<script lang="ts">
  import Buddy from '$lib/Buddy.svelte';
  import { repos, stats, type CiStatus, type Provider } from '$lib/data/stub';

  let activeFilter: 'waiting' | 'all' | 'releases' | 'local' = $state('all');

  const ciLabel: Record<CiStatus, string> = {
    ok: 'passing',
    fail: 'failing',
    run: 'running',
    none: 'no ci',
  };

  function providerInitial(p: Provider, owner: string): string {
    return owner.charAt(0).toUpperCase();
  }

  function providerClass(p: Provider): string {
    return ({
      github: 'gh',
      gitlab: 'gl',
      codeberg: 'cb',
      'mpsd-gitlab': 'mp',
    } as const)[p];
  }
</script>

<div class="app">

  <!-- Custom title bar (drag region). titleBarStyle: 'Overlay' on macOS
       means the traffic lights are drawn on top of this area at the left. -->
  <header class="titlebar" data-tauri-drag-region>
    <span class="tb-spacer" data-tauri-drag-region></span>
    <Buddy size={20} />
    <span class="brand" data-tauri-drag-region>gitBuddy</span>
    <span class="crumb" data-tauri-drag-region>/ <b>Waiting on me</b></span>
    <span class="tb-flex" data-tauri-drag-region></span>
    <span class="sync"><span class="dot" aria-hidden="true"></span>Synced 24 sec ago</span>
  </header>

  <!-- Toolbar with search + actions -->
  <div class="toolbar">
    <label class="search">
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
        <circle cx="11" cy="11" r="7" /><path d="m20 20-3.5-3.5" />
      </svg>
      <input type="text" placeholder="Search by repo, owner, label, anything…" />
      <span class="sho">⌘ K</span>
    </label>
    <button class="iconbtn" title="Refresh" aria-label="Refresh">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round">
        <path d="M21 12a9 9 0 1 1-3-6.7" /><path d="M21 4v5h-5" />
      </svg>
    </button>
    <button class="iconbtn bell" title="Notifications" aria-label="Notifications">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">
        <path d="M18 8a6 6 0 1 0-12 0c0 7-3 9-3 9h18s-3-2-3-9" />
        <path d="M13.7 21a2 2 0 0 1-3.4 0" />
      </svg>
    </button>
    <button class="iconbtn" title="Settings" aria-label="Settings">
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8">
        <circle cx="12" cy="12" r="3" />
        <path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1A1.7 1.7 0 0 0 4.6 9a1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z" />
      </svg>
    </button>
  </div>

  <div class="body">

    <!-- Sidebar -->
    <aside class="side">
      <section class="sec">
        <h3>What's <em>waiting</em></h3>
        <button
          class="pill"
          class:on={activeFilter === 'waiting'}
          onclick={() => (activeFilter = 'waiting')}
        >
          <span class="sw t"></span> On you <span class="c">{stats.waiting}</span>
        </button>
        <button
          class="pill"
          class:on={activeFilter === 'all'}
          onclick={() => (activeFilter = 'all')}
        >
          <span class="sw s"></span> All repos <span class="c">147</span>
        </button>
        <button
          class="pill"
          class:on={activeFilter === 'releases'}
          onclick={() => (activeFilter = 'releases')}
        >
          <span class="sw b"></span> New releases <span class="c">{stats.newReleases}</span>
        </button>
        <button
          class="pill"
          class:on={activeFilter === 'local'}
          onclick={() => (activeFilter = 'local')}
        >
          <span class="sw p"></span> Local clones <span class="c">{stats.localClones}</span>
        </button>
      </section>

      <section class="sec">
        <h3>Accounts</h3>
        <button class="pill"><span class="ava gh-p">b</span> bjoern.witt <span class="c">42</span></button>
        <button class="pill"><span class="ava gh-w">m</span> mpsd-bw <span class="c">63</span></button>
        <button class="pill"><span class="ava gl-p">g</span> gitlab.com <span class="c">11</span></button>
        <button class="pill"><span class="ava gl-w">M</span> mpsd.gitlab <span class="c">24</span></button>
        <button class="pill"><span class="ava cb">c</span> codeberg <span class="c">7</span></button>
      </section>
    </aside>

    <!-- Content -->
    <main class="content">
      <div class="greet-row">
        <h1>Hi, <em>Björn</em>.</h1>
        <p class="lede">
          You have <b>4 things needing attention</b> and <b>3 fresh releases</b> across 5 accounts.
        </p>
      </div>

      <div class="stats">
        <div class="stat t">
          <span class="lbl">Waiting on you</span>
          <span class="num">{stats.waiting}</span>
          <span class="delta">+3 since yesterday</span>
        </div>
        <div class="stat s">
          <span class="lbl">CI passing</span>
          <span class="num">{stats.ciPassing}<em>/{stats.ciTotal}</em></span>
          <span class="delta">3 failing · 4 running</span>
        </div>
        <div class="stat b">
          <span class="lbl">New releases</span>
          <span class="num">{stats.newReleases}</span>
          <span class="delta">in the last 7 days</span>
        </div>
        <div class="stat">
          <span class="lbl">Local clones</span>
          <span class="num">{stats.localClones}</span>
          <span class="delta">{stats.withUncommitted} with uncommitted</span>
        </div>
      </div>

      <h2 class="section-h">
        Your <em>repos</em>
        <span class="count">{repos.length} shown · 147 total</span>
        <button class="link">View all →</button>
      </h2>

      <div class="repo-grid">
        {#each repos as r (r.id)}
          <button class="card" class:featured={r.versionIsNew}>
            <span class="pchip {providerClass(r.provider)}">{providerInitial(r.provider, r.owner)}</span>
            <div class="rname">
              <span class="owner">{r.owner}</span> / <b>{r.name}</b>
              <div class="sub">
                <span class="pin">
                  <span class="d" class:off={!r.localPath}></span>
                  {r.localPath ?? 'not cloned'}
                </span>
                <span>{r.branch}</span>
                {#if r.warnings}
                  {#each r.warnings as w}
                    <span class="warn">{w}</span>
                  {/each}
                {/if}
              </div>
            </div>
            <div class="rmeta">
              {#if r.version}
                <span class="rver">
                  {r.version}
                  {#if r.versionIsNew}<span class="new">NEW</span>{/if}
                </span>
              {:else}
                <span class="rver">—</span>
              {/if}
              <span class="rci {r.ci}">
                <span class="b"></span> {ciLabel[r.ci]}
              </span>
              <span class="counts">
                {#if r.hotCount}<span class="hot">{r.issues}</span>{:else}{r.issues}{/if}
                issues · {r.prs} {r.provider === 'gitlab' || r.provider === 'mpsd-gitlab' ? 'MR' : 'PR'}{r.prs === 1 ? '' : 's'}
              </span>
            </div>
          </button>
        {/each}
      </div>
    </main>
  </div>
</div>

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
    background: var(--paper);
    overflow: hidden;
  }

  /* Title bar ------------------------------------------------------- */
  .titlebar {
    height: 46px;
    background: linear-gradient(180deg, #FDF7EA 0%, #F4E9D2 100%);
    border-bottom: 1px solid var(--line);
    display: flex;
    align-items: center;
    padding: 0 18px;
    gap: 14px;
    -webkit-user-select: none;
    user-select: none;
  }
  /* Reserve room for the macOS traffic lights on the left. */
  .tb-spacer { width: 60px; }
  .tb-flex   { flex: 1; }
  .brand {
    font-family: var(--font-display);
    font-size: 16px;
    letter-spacing: -0.01em;
    color: var(--ink);
  }
  .crumb {
    font-size: 12.5px;
    color: var(--ink-3);
  }
  .crumb b { color: var(--ink); font-weight: 500; }
  .sync {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    background: var(--cream-2);
    padding: 4px 10px;
    border-radius: 999px;
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .sync .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--sage);
  }

  /* Toolbar -------------------------------------------------------- */
  .toolbar {
    padding: 16px 18px;
    display: flex;
    align-items: center;
    gap: 12px;
    border-bottom: 1px solid var(--line);
    background: var(--paper-2);
  }
  .search {
    flex: 1;
    height: 38px;
    background: var(--paper);
    border-radius: var(--r-md);
    border: 1px solid var(--line-2);
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 0 14px;
    color: var(--ink-3);
    font-size: 13.5px;
    box-shadow: var(--shadow-1);
  }
  .search input {
    flex: 1;
    border: 0;
    background: transparent;
    outline: none;
    font: inherit;
    color: var(--ink);
  }
  .search input::placeholder { color: var(--ink-3); }
  .search .sho {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    background: var(--cream-2);
    padding: 2px 7px;
    border-radius: 5px;
  }
  .iconbtn {
    width: 38px;
    height: 38px;
    border-radius: var(--r-md);
    display: grid;
    place-items: center;
    color: var(--ink-2);
    background: var(--paper);
    border: 1px solid var(--line-2);
    box-shadow: var(--shadow-1);
    position: relative;
  }
  .iconbtn:hover { background: var(--cream-2); }
  .iconbtn.bell::after {
    content: '12';
    position: absolute;
    top: -4px;
    right: -4px;
    background: var(--terracotta);
    color: white;
    font-size: 9.5px;
    font-weight: 600;
    border-radius: 999px;
    padding: 2px 5px;
    box-shadow: 0 0 0 2px var(--paper-2);
    font-family: var(--font-mono);
  }

  /* Body ----------------------------------------------------------- */
  .body {
    flex: 1;
    display: grid;
    grid-template-columns: 240px 1fr;
    min-height: 0;
  }
  .side {
    padding: 22px 14px;
    border-right: 1px solid var(--line);
    background: var(--paper-2);
    overflow-y: auto;
  }
  .sec + .sec { margin-top: 20px; }
  .sec h3 {
    font-family: var(--font-display);
    font-size: 13.5px;
    color: var(--ink-2);
    padding: 0 8px 8px;
    margin: 0;
    letter-spacing: -0.005em;
    font-weight: 400;
  }
  .sec h3 em {
    font-style: italic;
    color: var(--terracotta);
  }
  .pill {
    width: 100%;
    padding: 9px 12px;
    border-radius: var(--r-md);
    display: flex;
    align-items: center;
    gap: 10px;
    color: var(--ink-2);
    font-size: 13.5px;
    text-align: left;
    margin-bottom: 2px;
  }
  .pill:hover { background: rgba(0, 0, 0, 0.025); }
  .pill.on {
    background: var(--terracotta-soft);
    color: var(--ink);
    font-weight: 600;
  }
  .pill .c {
    margin-left: auto;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
  }
  .pill.on .c { color: var(--terracotta); }
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  .sw.t { background: var(--terracotta); }
  .sw.s { background: var(--sage); }
  .sw.b { background: var(--butter); }
  .sw.p { background: var(--plum); }

  .ava {
    width: 22px;
    height: 22px;
    border-radius: var(--r-sm);
    display: grid;
    place-items: center;
    color: white;
    font-weight: 600;
    font-size: 11px;
    font-family: var(--font-display);
    flex-shrink: 0;
  }
  .ava.gh-p { background: linear-gradient(135deg, #6B5A4D, #2E211B); }
  .ava.gh-w { background: linear-gradient(135deg, #80987B, #4A5E48); }
  .ava.gl-p { background: linear-gradient(135deg, #E8A06A, #C66243); }
  .ava.gl-w { background: linear-gradient(135deg, #B6A5C9, #6E5E80); }
  .ava.cb   { background: linear-gradient(135deg, #8DBBC9, #4E7A8A); }

  /* Content -------------------------------------------------------- */
  .content {
    padding: 26px 30px 30px;
    overflow-y: auto;
    overflow-x: hidden;
  }
  .greet-row {
    display: flex;
    align-items: flex-end;
    gap: 14px;
    margin-bottom: 18px;
  }
  .greet-row h1 {
    font-family: var(--font-display);
    font-size: 32px;
    letter-spacing: -0.02em;
    line-height: 1;
    color: var(--ink);
    margin: 0;
    font-weight: 400;
  }
  .greet-row h1 em {
    font-style: italic;
    color: var(--terracotta);
  }
  .greet-row .lede {
    margin: 0 0 6px;
    font-size: 13.5px;
    color: var(--ink-3);
  }
  .greet-row .lede b { color: var(--ink-2); font-weight: 500; }

  .stats {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 12px;
    margin-bottom: 26px;
  }
  .stat {
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: var(--r-lg);
    padding: 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .stat .lbl { font-size: 11.5px; color: var(--ink-3); }
  .stat .num {
    font-family: var(--font-display);
    font-size: 34px;
    letter-spacing: -0.02em;
    line-height: 1;
    color: var(--ink);
  }
  .stat .num em { font-style: italic; }
  .stat .delta {
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--ink-3);
    margin-top: 4px;
  }
  .stat.t {
    background: linear-gradient(135deg, #FBE6DA 0%, #F6D7C2 100%);
    border-color: rgba(198, 98, 67, 0.12);
  }
  .stat.t .num { color: var(--terracotta); }
  .stat.s {
    background: linear-gradient(135deg, #E7EDD9 0%, #DCE7CD 100%);
    border-color: rgba(128, 152, 123, 0.18);
  }
  .stat.s .num { color: #5E7758; }
  .stat.b {
    background: linear-gradient(135deg, #FBEED1 0%, #F4E0AE 100%);
    border-color: rgba(232, 185, 75, 0.2);
  }
  .stat.b .num { color: #B68C2C; }

  .section-h {
    font-family: var(--font-display);
    font-size: 20px;
    color: var(--ink);
    letter-spacing: -0.01em;
    margin: 0 0 12px;
    display: flex;
    align-items: baseline;
    font-weight: 400;
  }
  .section-h em {
    font-style: italic;
    color: var(--terracotta);
    margin-left: 4px;
  }
  .section-h .count {
    margin-left: 8px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-3);
  }
  .section-h .link {
    margin-left: auto;
    font-size: 12px;
    color: var(--terracotta);
    font-family: var(--font-body);
  }

  .repo-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
  }
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
  }
  .card:hover {
    transform: translateY(-1px);
    box-shadow: var(--shadow-2);
  }
  .card.featured {
    background: linear-gradient(135deg, #FFF9EB 0%, #FBEFD0 100%);
    border-color: rgba(232, 185, 75, 0.3);
  }
  .pchip {
    width: 28px;
    height: 28px;
    border-radius: 9px;
    display: grid;
    place-items: center;
    background: var(--cream-2);
    color: var(--ink-2);
    font-weight: 700;
    font-size: 11px;
    font-family: var(--font-display);
  }
  .pchip.gh { background: #2E211B; color: #FAF4EA; }
  .pchip.gl { background: linear-gradient(135deg, #E89C5C, #C66243); color: white; }
  .pchip.cb { background: linear-gradient(135deg, #8DBBC9, #4E7A8A); color: white; }
  .pchip.mp { background: linear-gradient(135deg, #B6A5C9, #6E5E80); color: white; }
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
  }
  .rname .sub .pin .d {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: var(--sage);
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
  .rver {
    font-family: var(--font-display);
    font-size: 14px;
    color: var(--ink);
    letter-spacing: -0.005em;
  }
  .rver .new {
    display: inline-block;
    font-family: var(--font-mono);
    font-size: 9.5px;
    color: var(--terracotta);
    background: var(--terracotta-soft);
    padding: 1px 6px;
    border-radius: 999px;
    margin-left: 4px;
    vertical-align: 2px;
    letter-spacing: 0.06em;
  }
  .rci {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    font-family: var(--font-mono);
  }
  .rci .b {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--sage);
  }
  .rci.fail { color: var(--terracotta); }
  .rci.fail .b { background: var(--terracotta); }
  .rci.run { color: #B68C2C; }
  .rci.run .b {
    background: var(--butter);
    animation: rci 1.4s ease-in-out infinite;
  }
  .rci.none { color: var(--ink-3); opacity: 0.5; }
  .rci.none .b { background: var(--ink-4); }
  @keyframes rci {
    0%, 100% { opacity: 1; transform: scale(1); }
    50%      { opacity: 0.5; transform: scale(0.8); }
  }
  .counts {
    display: flex;
    gap: 4px;
    font-family: var(--font-mono);
    font-size: 11.5px;
    color: var(--ink-2);
  }
  .counts .hot { color: var(--terracotta); font-weight: 600; }

  /* Responsive collapse for narrow windows */
  @media (max-width: 720px) {
    .body { grid-template-columns: 1fr; }
    .side { display: none; }
    .stats { grid-template-columns: repeat(2, 1fr); }
    .repo-grid { grid-template-columns: 1fr; }
  }
</style>
