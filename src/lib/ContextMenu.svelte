<!--
  Floating context-menu popup. Positioned at a screen-space coordinate
  (typically a right-click event) and clamped inside the popover bounds.

  Usage:
    <ContextMenu
      bind:open={menuOpen}
      x={menuX}
      y={menuY}
      items={[
        { label: 'Open in browser', onclick: () => ... },
        { separator: true },
        { label: 'Show in Finder', onclick: () => ... },
      ]}
    />

  Each item is either an action ({ label, onclick }) or a separator
  ({ separator: true }). Disabled items can set `disabled: true`.
-->
<script lang="ts">
  import { onMount } from 'svelte';

  export type MenuItem =
    | { label: string; onclick: () => void; disabled?: boolean; danger?: boolean }
    | { separator: true };

  interface Props {
    open: boolean;
    x: number;
    y: number;
    items: MenuItem[];
  }

  let { open = $bindable(), x, y, items }: Props = $props();

  let el: HTMLDivElement | undefined = $state();

  // Clamp the menu inside the popover so it doesn't overflow when right-
  // clicked near the right/bottom edge of the panel.
  let pos = $derived.by(() => {
    if (!open) return { left: x, top: y };
    const w = el?.offsetWidth ?? 200;
    const h = el?.offsetHeight ?? 200;
    const maxW = window.innerWidth - 8;
    const maxH = window.innerHeight - 8;
    return {
      left: Math.max(8, Math.min(x, maxW - w)),
      top: Math.max(8, Math.min(y, maxH - h)),
    };
  });

  function handleClickOutside(e: MouseEvent) {
    if (!open) return;
    if (el && !el.contains(e.target as Node)) {
      open = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && open) {
      open = false;
    }
  }

  onMount(() => {
    document.addEventListener('mousedown', handleClickOutside, true);
    document.addEventListener('keydown', handleKeydown);
    return () => {
      document.removeEventListener('mousedown', handleClickOutside, true);
      document.removeEventListener('keydown', handleKeydown);
    };
  });

  function pick(item: MenuItem) {
    if ('separator' in item) return;
    if (item.disabled) return;
    open = false;
    // Defer execution so the menu closes visually before any UI churn from
    // the action (e.g. clipboard toast, focus jumping to a new window).
    queueMicrotask(item.onclick);
  }
</script>

{#if open}
  <div
    bind:this={el}
    class="ctx-menu"
    style:left={`${pos.left}px`}
    style:top={`${pos.top}px`}
    role="menu"
    tabindex="-1"
  >
    {#each items as item, i (i)}
      {#if 'separator' in item}
        <div class="ctx-sep" role="separator"></div>
      {:else}
        <button
          type="button"
          class="ctx-item"
          class:danger={item.danger}
          disabled={item.disabled}
          onclick={() => pick(item)}
          role="menuitem"
        >
          {item.label}
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .ctx-menu {
    position: fixed;
    z-index: 200;
    min-width: 180px;
    background: var(--paper);
    border-radius: var(--r-md);
    border: 1px solid var(--line);
    box-shadow:
      0 0 0 0.5px rgba(46, 33, 27, 0.10),
      0 8px 24px -6px rgba(60, 40, 20, 0.28);
    padding: 4px;
    font-size: 13px;
  }
  .ctx-item {
    display: block;
    width: 100%;
    text-align: left;
    padding: 6px 10px;
    border-radius: var(--r-sm);
    color: var(--ink);
    background: transparent;
    cursor: pointer;
    font: inherit;
  }
  .ctx-item:hover:not(:disabled) {
    background: var(--cream-2);
  }
  .ctx-item:disabled {
    color: var(--ink-4);
    cursor: default;
  }
  .ctx-item.danger { color: var(--plum); }
  .ctx-item.danger:hover:not(:disabled) { background: var(--plum-soft); }
  .ctx-sep {
    height: 1px;
    margin: 4px 0;
    background: var(--line);
  }
</style>
