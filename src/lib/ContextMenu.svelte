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

  /** Indices of the items that can actually be focused — separators and
   *  disabled entries are skipped by arrow navigation. */
  let focusableIndices = $derived(
    items
      .map((item, i) => (!('separator' in item) && !item.disabled ? i : -1))
      .filter((i) => i >= 0),
  );

  /** Buttons keyed by their index in `items`, so arrow keys can move focus. */
  let itemEls: Record<number, HTMLButtonElement> = {};

  /** Element that had focus when the menu opened, restored on close. Without
   *  this, dismissing the menu drops focus to the document and the keyboard
   *  user loses their place in the list. */
  let previouslyFocused: HTMLElement | null = null;

  function focusItemAt(position: number) {
    const idx = focusableIndices.at(position);
    if (idx === undefined) return;
    itemEls[idx]?.focus();
  }

  /** Where the currently focused item sits within `focusableIndices`. */
  function currentPosition(): number {
    const active = document.activeElement;
    return focusableIndices.findIndex((i) => itemEls[i] === active);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (!open) return;
    switch (e.key) {
      case 'Escape':
        open = false;
        break;
      case 'ArrowDown': {
        e.preventDefault();
        const next = currentPosition() + 1;
        focusItemAt(next >= focusableIndices.length ? 0 : next);
        break;
      }
      case 'ArrowUp': {
        e.preventDefault();
        const prev = currentPosition() - 1;
        focusItemAt(prev < 0 ? focusableIndices.length - 1 : prev);
        break;
      }
      case 'Home':
        e.preventDefault();
        focusItemAt(0);
        break;
      case 'End':
        e.preventDefault();
        focusItemAt(focusableIndices.length - 1);
        break;
    }
  }

  // Move focus into the menu when it opens and hand it back when it closes.
  // The markup already claimed `role="menu"`, which tells assistive tech this
  // is a focus-managed widget — but nothing ever moved focus, so Tab walked
  // straight past the menu into the page behind it and VoiceOver announced
  // nothing at all when it appeared.
  $effect(() => {
    if (!open) return;
    previouslyFocused = document.activeElement as HTMLElement | null;
    // After the {#if} block has rendered the buttons.
    queueMicrotask(() => focusItemAt(0));
    return () => {
      previouslyFocused?.focus?.();
      previouslyFocused = null;
    };
  });

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
          bind:this={itemEls[i]}
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
  /* Arrow-key navigation moves focus, so the focused item has to look
     selected — hover alone leaves keyboard users with no cursor. */
  .ctx-item:focus-visible {
    background: var(--cream-2);
    outline: 2px solid var(--terracotta);
    outline-offset: -2px;
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
