<!--
  Buddy mascot — a sprout. Used in the menu bar tray, main window header,
  popover header, and any empty/onboarding states.
  Reference: design-mockups/03-buddy-warm.html (sprout SVG symbol).
-->
<script lang="ts">
  interface Props {
    size?: number;
    /** Render a flat single-color silhouette (used for the tray template icon). */
    silhouette?: boolean;
  }

  let { size = 24, silhouette = false }: Props = $props();
</script>

{#if silhouette}
  <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
    <circle cx="16" cy="20" r="9" fill="currentColor" />
    <path d="M16 11 Q 13 6 9 5 Q 10 9 13 11 Z" fill="currentColor" />
    <path d="M16 11 Q 19 7 24 7 Q 22 11 18 12 Z" fill="currentColor" />
  </svg>
{:else}
  <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
    <!-- terracotta body -->
    <circle cx="16" cy="20" r="9" fill="var(--terracotta, #C66243)" />
    <!-- sage leaves -->
    <path d="M16 11 Q 13 6 9 5 Q 10 9 13 11 Z" fill="var(--sage, #80987B)" />
    <path d="M16 11 Q 19 7 24 7 Q 22 11 18 12 Z" fill="var(--sage, #80987B)" />
    <!-- eyes -->
    <circle cx="13" cy="20" r="1.4" fill="var(--cream, #FAF4EA)" />
    <circle cx="19" cy="20" r="1.4" fill="var(--cream, #FAF4EA)" />
    <!-- smile -->
    <path
      d="M14 23 Q 16 24.6 18 23"
      stroke="var(--cream, #FAF4EA)"
      stroke-width="1.1"
      fill="none"
      stroke-linecap="round"
    />
  </svg>
{/if}
