<script lang="ts">
  // Detail pane of the main window: header + quick actions, clone section
  // (existing clones or the inline clone form), CI, latest release, and the
  // repo's waiting items. Extracted from routes/+page.svelte; the parent
  // renders it inside `.body.has-detail`'s third grid column and recreates
  // it per repo via {#key}, which is what resets the clone-form state on
  // selection change. Shared visual vocabulary (.row, .kind-chip, .pchip,
  // .rci, badges) comes from routes/main-window.css.
  import { onDestroy } from 'svelte';
  import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
  import { open as openDialog } from '@tauri-apps/plugin-dialog';
  import { writeText } from '@tauri-apps/plugin-clipboard-manager';
  import {
    cloneRepo,
    runEditor,
    runTerminal,
    providerChipText,
    providerCssClass,
    providerLabel,
    type Repo,
    type LocalRepo,
    type CiRun,
    type Release,
    type WaitingItem,
  } from '$lib/data/api';

  interface Props {
    repo: Repo;
    /** Local clones joined onto this repo (empty = remote-only). */
    localDiag: LocalRepo[];
    ci: CiRun | null;
    release: Release | null;
    /** Waiting items filtered to this repo. */
    items: WaitingItem[];
    editorCmd: string;
    terminalCmd: string;
    /** Account whose token clone_repo should use — picked by the parent
     *  (most recently added account that surfaces this repo). */
    cloneAccountId: string | null;
    /** Pre-fill for the clone form's parent directory (first scan root). */
    defaultCloneParentDir: string;
    hasScanRoots: boolean;
    onclose: () => void;
    onOpenSettings: () => void;
    /** Called after a successful clone so the parent can rescan locals. */
    onCloned: () => Promise<void> | void;
    onItemContextMenu: (e: MouseEvent, item: WaitingItem) => void;
  }

  let {
    repo: r,
    localDiag,
    ci,
    release,
    items,
    editorCmd,
    terminalCmd,
    cloneAccountId,
    defaultCloneParentDir,
    hasScanRoots,
    onclose,
    onOpenSettings,
    onCloned,
    onItemContextMenu,
  }: Props = $props();

  let firstLocal = $derived(localDiag[0]);

  // ── Clone form (component-local: a fresh instance per repo via {#key}
  //    in the parent resets all of this on selection change) ─────────────
  type CloneStatus = 'idle' | 'cloning' | 'success' | 'error';
  let cloneFormOpen = $state(false);
  let cloneParentDir = $state('');
  let cloneFolderName = $state('');
  let cloneStatus: CloneStatus = $state('idle');
  let cloneMessage = $state('');
  let autoCloseHandle: ReturnType<typeof setTimeout> | null = null;
  onDestroy(() => {
    if (autoCloseHandle) clearTimeout(autoCloseHandle);
  });

  function openCloneForm() {
    cloneFormOpen = true;
    cloneStatus = 'idle';
    cloneMessage = '';
    cloneParentDir = defaultCloneParentDir;
    cloneFolderName = r.name;
  }

  function closeCloneForm() {
    cloneFormOpen = false;
    cloneStatus = 'idle';
    cloneMessage = '';
  }

  async function browseCloneParentDir() {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        defaultPath: cloneParentDir || undefined,
        title: 'Choose where to clone',
      });
      if (typeof picked === 'string' && picked.length > 0) {
        cloneParentDir = picked;
      }
    } catch (e) {
      cloneStatus = 'error';
      cloneMessage = `Folder picker failed: ${e}`;
    }
  }

  async function doClone() {
    if (!r.clone_url) {
      cloneStatus = 'error';
      cloneMessage = 'This repo has no HTTPS clone URL.';
      return;
    }
    if (!cloneParentDir.trim() || !cloneFolderName.trim()) return;
    cloneStatus = 'cloning';
    cloneMessage = '';
    try {
      const path = await cloneRepo(
        r.clone_url,
        cloneParentDir.trim(),
        cloneFolderName.trim(),
        cloneAccountId,
      );
      cloneStatus = 'success';
      cloneMessage = path;
      // Refresh the local scan so the new clone gets joined onto its
      // remote repo across the UI.
      await onCloned();
      // Auto-close after a short beat so the user sees the success
      // message but doesn't have to dismiss it.
      autoCloseHandle = setTimeout(() => {
        if (cloneStatus === 'success') closeCloneForm();
      }, 1800);
    } catch (e) {
      cloneStatus = 'error';
      cloneMessage = String(e);
    }
  }

  async function openExternal(url: string) {
    try {
      await openUrl(url);
    } catch {
      // Opener plugin failure is non-fatal — swallow rather than poison
      // the pane with an error over a missing browser handler.
    }
  }
</script>

<aside class="detail-pane" aria-label="Repo details">
  <header class="dp-header">
    <span class="pchip dp-pchip {providerCssClass(r.provider)}">{providerChipText(r)}</span>
    <div class="dp-titles">
      <h2 class="dp-name">
        <span class="dp-owner">{r.owner}/</span><span class="dp-rname">{r.name}</span>
      </h2>
      <div class="dp-meta">
        <span>{providerLabel({ provider: r.provider, html_url: r.html_url })}</span>
        {#if r.is_private}<span class="dp-badge">private</span>{/if}
        {#if r.is_fork}<span class="dp-badge">fork</span>{/if}
        <span class="dp-branch">{r.default_branch}</span>
      </div>
    </div>
    <button
      type="button"
      class="dp-close"
      onclick={onclose}
      aria-label="Close detail pane"
      data-tip="Close (Esc)"
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
        <path d="M18 6 6 18" />
        <path d="m6 6 12 12" />
      </svg>
    </button>
  </header>

  {#if r.description}
    <p class="dp-desc">{r.description}</p>
  {/if}

  <div class="dp-actions">
    <button
      type="button"
      class="dp-action primary"
      onclick={() => openExternal(r.html_url)}
    >
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
        <path d="M7 17 17 7" />
        <path d="M7 7h10v10" />
      </svg>
      Open in browser
    </button>
    {#if firstLocal}
      <button
        type="button"
        class="dp-action"
        onclick={() => revealItemInDir(firstLocal.path)}
        data-tip="Reveal in Finder"
      >
        Show in Finder
      </button>
      {#if editorCmd.length > 0}
        <button
          type="button"
          class="dp-action"
          onclick={() => runEditor(firstLocal.path)}
          data-tip="Open with {editorCmd}"
        >
          Open in {editorCmd}
        </button>
      {/if}
      {#if terminalCmd.length > 0}
        <button
          type="button"
          class="dp-action"
          onclick={() => runTerminal(firstLocal.path)}
          data-tip="Open with {terminalCmd}"
        >
          Open in {terminalCmd}
        </button>
      {/if}
    {/if}
    {#if r.clone_url}
      {@const cloneUrl = r.clone_url}
      <button
        type="button"
        class="dp-action"
        onclick={() => writeText(cloneUrl)}
        data-tip="Copy to clipboard"
      >
        Copy HTTPS
      </button>
    {/if}
    {#if r.ssh_url}
      {@const sshUrl = r.ssh_url}
      <button
        type="button"
        class="dp-action"
        onclick={() => writeText(sshUrl)}
        data-tip="Copy to clipboard"
      >
        Copy SSH
      </button>
    {/if}
  </div>

  <section class="dp-sec">
    <h3 class="dp-sec-h">Clone</h3>
    {#if localDiag.length === 0}
      {@const canClone = !!r.clone_url}
      {#if !cloneFormOpen}
        <p class="dp-empty">
          Not cloned locally.
          {#if !hasScanRoots}
            Add a folder in <button
              type="button"
              class="link-inline"
              onclick={onOpenSettings}
            >Settings</button> for gitBuddy to find local copies.
          {/if}
        </p>
        {#if canClone}
          <button
            type="button"
            class="dp-action dp-clone-start"
            onclick={openCloneForm}
          >
            Clone repository…
          </button>
        {/if}
      {:else}
        <div class="dp-clone-form">
          <label class="dp-clone-field">
            <span class="lbl">Parent directory</span>
            <div class="dp-clone-row-input">
              <input
                type="text"
                bind:value={cloneParentDir}
                placeholder="~/Developer"
                spellcheck="false"
                autocomplete="off"
                disabled={cloneStatus === 'cloning'}
              />
              <button
                type="button"
                class="dp-clone-browse"
                onclick={browseCloneParentDir}
                disabled={cloneStatus === 'cloning'}
              >
                Browse…
              </button>
            </div>
          </label>
          <label class="dp-clone-field">
            <span class="lbl">Folder name</span>
            <input
              type="text"
              bind:value={cloneFolderName}
              placeholder={r.name}
              spellcheck="false"
              autocomplete="off"
              disabled={cloneStatus === 'cloning'}
            />
          </label>
          <p class="dp-clone-target">
            Target:
            <code>{(cloneParentDir || '~').replace(/\/$/, '')}/{cloneFolderName || r.name}</code>
          </p>

          {#if cloneStatus === 'error'}
            <p class="dp-clone-err">{cloneMessage}</p>
          {:else if cloneStatus === 'success'}
            <p class="dp-clone-ok">Cloned to <code>{cloneMessage}</code></p>
          {/if}

          <div class="dp-clone-actions">
            <button
              type="button"
              class="dp-clone-cancel"
              onclick={closeCloneForm}
              disabled={cloneStatus === 'cloning'}
            >
              Cancel
            </button>
            <button
              type="button"
              class="dp-clone-go"
              onclick={doClone}
              disabled={cloneStatus === 'cloning'
                || !cloneParentDir.trim()
                || !cloneFolderName.trim()
                || cloneStatus === 'success'}
            >
              {cloneStatus === 'cloning' ? 'Cloning…' : 'Clone'}
            </button>
          </div>
        </div>
      {/if}
    {:else}
      {#each localDiag as l (l.path)}
        {@const dirty = l.dirty_staged + l.dirty_unstaged}
        <div class="dp-clone">
          <div class="dp-clone-path" title={l.path}>{l.path}</div>
          <div class="dp-clone-row">
            <span class="dp-clone-branch">
              <span class="d" class:off={l.detached}></span>
              {l.branch ?? (l.detached ? 'detached' : '—')}
            </span>
            {#if l.ahead > 0}<span class="dp-clone-stat warn">{l.ahead} ahead</span>{/if}
            {#if l.behind > 0}<span class="dp-clone-stat warn">{l.behind} behind</span>{/if}
            {#if dirty > 0}<span class="dp-clone-stat warn">{dirty} uncommitted</span>{/if}
            {#if l.untracked > 0}<span class="dp-clone-stat">{l.untracked} untracked</span>{/if}
            {#if l.ahead === 0 && l.behind === 0 && dirty === 0 && l.untracked === 0}
              <span class="dp-clone-stat clean">clean</span>
            {/if}
          </div>
        </div>
      {/each}
    {/if}
  </section>

  <section class="dp-sec">
    <h3 class="dp-sec-h">CI</h3>
    {#if ci === null || ci.status === 'none'}
      <p class="dp-empty">No recent workflow runs.</p>
    {:else}
      <div class="dp-ci">
        <span class="rci {ci.status}">
          <span class="b"></span>
          {#if ci.status === 'ok'}passing
          {:else if ci.status === 'fail'}failing
          {:else if ci.status === 'run'}running
          {:else if ci.status === 'cancelled'}cancelled
          {/if}
        </span>
        <span class="dp-ci-meta">
          {ci.workflow_name ?? 'workflow'}
          {#if ci.branch}<span class="dp-ci-branch">on {ci.branch}</span>{/if}
        </span>
        {#if ci.html_url}
          {@const ciUrl = ci.html_url}
          <button
            type="button"
            class="dp-link"
            onclick={() => openExternal(ciUrl)}
          >View run →</button>
        {/if}
      </div>
    {/if}
  </section>

  <section class="dp-sec">
    <h3 class="dp-sec-h">Latest release</h3>
    {#if release === null}
      <p class="dp-empty">No releases published.</p>
    {:else}
      {@const rel = release}
      <div class="dp-release">
        <div class="dp-release-title">
          <span class="dp-release-name">{rel.name || rel.tag}</span>
          {#if rel.is_prerelease}<span class="badge-pre">pre</span>{/if}
          {#if rel.is_new}<span class="new-badge">NEW</span>{/if}
        </div>
        <div class="dp-release-meta">
          <span class="row-tag">{rel.tag}</span>
          <span class="row-dot">·</span>
          <span>{rel.age_human}</span>
          <button
            type="button"
            class="dp-link"
            onclick={() => openExternal(rel.html_url)}
          >View release →</button>
        </div>
      </div>
    {/if}
  </section>

  <section class="dp-sec">
    <h3 class="dp-sec-h">
      Waiting on you
      {#if items.length > 0}
        <span class="dp-sec-count">{items.length}</span>
      {/if}
    </h3>
    {#if items.length === 0}
      <p class="dp-empty">Nothing waiting in this repo.</p>
    {:else}
      <div class="dp-items">
        {#each items as it (it.id)}
          <button
            type="button"
            class="row dp-item"
            onclick={() => openExternal(it.url)}
            oncontextmenu={(e) => onItemContextMenu(e, it)}
          >
            <span class="kind-chip {it.kind.toLowerCase()}">{it.kind}</span>
            <span class="row-body">
              <span class="row-title">{it.title}</span>
              <span class="row-meta">
                <span class="row-reason">{it.reason === 'review' ? 'review requested' : it.reason}</span>
              </span>
            </span>
            <span class="row-age">{it.age_human}</span>
          </button>
        {/each}
      </div>
    {/if}
  </section>
</aside>

<style>
/* Detail pane --------------------------------------------------- */
.detail-pane {
  border-left: 1px solid var(--line);
  background: var(--paper-2);
  padding: 22px 24px 26px;
  overflow-y: auto;
  overflow-x: hidden;
  display: flex;
  flex-direction: column;
  gap: 18px;
  animation: dp-in 0.18s ease-out;
}
@keyframes dp-in {
  from {
    opacity: 0;
    transform: translateX(8px);
  }
  to {
    opacity: 1;
    transform: translateX(0);
  }
}


.dp-header {
  display: grid;
  grid-template-columns: 36px 1fr auto;
  gap: 12px;
  align-items: start;
}
.dp-pchip {
  width: 36px;
  height: 36px;
  border-radius: 10px;
  font-size: 13px;
}
.dp-titles {
  min-width: 0;
}
.dp-name {
  font-family: var(--font-display);
  font-size: 22px;
  font-weight: 400;
  letter-spacing: -0.02em;
  color: var(--ink);
  margin: 0 0 4px;
  line-height: 1.15;
  word-break: break-word;
}
.dp-owner { color: var(--ink-3); }
.dp-rname { font-weight: 600; }
.dp-meta {
  font-family: var(--font-mono);
  font-size: 11px;
  color: var(--ink-3);
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.dp-badge {
  background: var(--cream-2);
  color: var(--ink-2);
  padding: 1px 6px;
  border-radius: 4px;
}
.dp-branch {
  background: var(--cream-2);
  color: var(--ink-2);
  padding: 1px 6px;
  border-radius: 4px;
}
.dp-close {
  width: 28px;
  height: 28px;
  border-radius: var(--r-sm);
  border: 0;
  background: transparent;
  color: var(--ink-3);
  cursor: pointer;
  display: grid;
  place-items: center;
  transition: background 0.12s ease, color 0.12s ease;
}
.dp-close:hover {
  background: var(--cream-2);
  color: var(--terracotta);
}

.dp-desc {
  margin: 0;
  font-size: 13.5px;
  color: var(--ink-2);
  line-height: 1.55;
}

.dp-actions {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
}
.dp-action {
  height: 30px;
  padding: 0 12px;
  background: var(--paper);
  border: 1px solid var(--line-2);
  border-radius: var(--r-sm);
  font-size: 12.5px;
  color: var(--ink-2);
  cursor: pointer;
  display: inline-flex;
  align-items: center;
  gap: 5px;
  transition: background 0.12s ease, color 0.12s ease, border-color 0.12s ease;
}
.dp-action:hover {
  background: var(--cream-2);
  border-color: var(--terracotta);
  color: var(--terracotta);
}
.dp-action.primary {
  background: var(--terracotta);
  color: white;
  border-color: transparent;
}
.dp-action.primary:hover {
  background: #B05738;
  color: white;
  border-color: transparent;
}

.dp-sec {
  border-top: 1px solid var(--line);
  padding-top: 16px;
  display: flex;
  flex-direction: column;
  gap: 10px;
}
.dp-sec-h {
  font-family: var(--font-display);
  font-size: 13px;
  font-weight: 400;
  color: var(--ink-3);
  letter-spacing: 0.04em;
  text-transform: uppercase;
  margin: 0;
  display: flex;
  align-items: center;
  gap: 8px;
}
.dp-sec-count {
  font-family: var(--font-mono);
  font-size: 11px;
  background: var(--terracotta-soft);
  color: var(--terracotta);
  padding: 1px 7px;
  border-radius: 999px;
  text-transform: none;
  letter-spacing: 0;
  font-weight: 600;
}
.dp-empty {
  margin: 0;
  color: var(--ink-3);
  font-size: 12.5px;
  font-style: italic;
}

/* Clone-from-detail-pane form — shown when a remote-only repo is
   selected. Lives in the same Clone section as the existing diagnostics
   rows so the user's eye stays in one place. */
.dp-clone-start {
  margin-top: 8px;
}
.dp-clone-form {
  display: flex;
  flex-direction: column;
  gap: 10px;
  margin-top: 4px;
}
.dp-clone-field {
  display: flex;
  flex-direction: column;
  gap: 4px;
}
.dp-clone-field .lbl {
  font-size: 11px;
  color: var(--ink-3);
  font-family: var(--font-mono);
  letter-spacing: 0.04em;
  text-transform: uppercase;
}
.dp-clone-field input {
  height: 30px;
  padding: 0 10px;
  border: 1px solid var(--line-2);
  border-radius: var(--r-sm);
  font: inherit;
  font-family: var(--font-mono);
  font-size: 12px;
  background: var(--paper-2);
  color: var(--ink);
  outline: none;
  transition: border-color 0.15s, background 0.15s;
}
.dp-clone-field input:focus {
  border-color: var(--terracotta);
  background: var(--paper);
}
.dp-clone-row-input {
  display: flex;
  gap: 6px;
}
.dp-clone-row-input input { flex: 1; }
.dp-clone-browse {
  height: 30px;
  padding: 0 12px;
  background: transparent;
  border: 1px solid var(--line-2);
  border-radius: var(--r-sm);
  font-size: 12px;
  color: var(--ink-2);
  cursor: pointer;
  transition: border-color 0.15s, color 0.15s;
}
.dp-clone-browse:hover {
  border-color: var(--terracotta);
  color: var(--terracotta);
}
.dp-clone-target {
  margin: 0;
  font-size: 11.5px;
  color: var(--ink-3);
}
.dp-clone-target code {
  font-family: var(--font-mono);
  color: var(--ink-2);
  background: var(--cream-2);
  padding: 1px 5px;
  border-radius: 4px;
  word-break: break-all;
}
.dp-clone-err {
  margin: 0;
  font-size: 12px;
  color: var(--terracotta);
}
.dp-clone-ok {
  margin: 0;
  font-size: 12px;
  color: var(--sage);
}
.dp-clone-ok code {
  font-family: var(--font-mono);
  color: var(--ink);
  background: var(--sage-soft);
  padding: 1px 5px;
  border-radius: 4px;
  word-break: break-all;
}
.dp-clone-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}
.dp-clone-cancel {
  height: 30px;
  padding: 0 12px;
  background: transparent;
  border: 1px solid var(--line-2);
  border-radius: var(--r-sm);
  color: var(--ink-2);
  font-size: 12px;
  cursor: pointer;
}
.dp-clone-go {
  height: 30px;
  padding: 0 14px;
  background: var(--terracotta);
  color: var(--paper);
  border: 0;
  border-radius: var(--r-sm);
  font-size: 12px;
  font-weight: 600;
  cursor: pointer;
  transition: background 0.15s, opacity 0.15s;
}
.dp-clone-go:hover:not(:disabled) { background: #B05738; }
.dp-clone-go:disabled { opacity: 0.5; cursor: default; }

.dp-clone {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.dp-clone + .dp-clone { margin-top: 8px; }
.dp-clone-path {
  font-family: var(--font-mono);
  font-size: 11.5px;
  color: var(--ink-2);
  background: var(--paper);
  border: 1px solid var(--line);
  border-radius: var(--r-sm);
  padding: 6px 10px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.dp-clone-row {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
  font-family: var(--font-mono);
  font-size: 11.5px;
  color: var(--ink-3);
}
.dp-clone-branch {
  display: inline-flex;
  align-items: center;
  gap: 5px;
  color: var(--ink-2);
}
.dp-clone-branch .d {
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: var(--sage);
}
.dp-clone-branch .d.off { background: var(--ink-4); }
.dp-clone-stat { color: var(--ink-3); }
.dp-clone-stat.warn { color: var(--terracotta); }
.dp-clone-stat.clean { color: var(--sage); }

.dp-ci {
  display: flex;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}
.dp-ci-meta {
  font-family: var(--font-mono);
  font-size: 11.5px;
  color: var(--ink-2);
  display: inline-flex;
  align-items: center;
  gap: 6px;
}
.dp-ci-branch {
  color: var(--ink-3);
}

.dp-release {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.dp-release-title {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
}
.dp-release-name {
  font-size: 14px;
  color: var(--ink);
  font-weight: 500;
}
.dp-release-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  font-family: var(--font-mono);
  font-size: 11.5px;
  color: var(--ink-3);
  flex-wrap: wrap;
}

.dp-link {
  background: transparent;
  border: 0;
  padding: 0;
  color: var(--terracotta);
  font-size: 12px;
  cursor: pointer;
  margin-left: auto;
}
.dp-link:hover { text-decoration: underline; }

.dp-items {
  display: flex;
  flex-direction: column;
  gap: 6px;
}
.dp-item {
  padding: 10px 12px;
  grid-template-columns: 28px 1fr auto;
  gap: 10px;
}
.dp-item .kind-chip {
  width: 26px;
  height: 26px;
  font-size: 10px;
}
.dp-item .row-title {
  font-size: 13px;
  -webkit-line-clamp: 2;
  line-clamp: 2;
}
</style>
