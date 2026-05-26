<!--
  Buddy mascot — a friendly robot with a git-branch antenna. Used in the
  menu-bar popover header, main-window titlebar, onboarding empty states,
  and anywhere the app needs to identify itself in chrome.
  Source: assets/robot_git_icon_sketch.svg (full) and assets/robot_git_menubar.svg
  (silhouette form). Update those + this file together when the mascot
  changes — the macOS tray icon is regenerated separately via
  scripts/regenerate-tray-icon.py.
-->
<script lang="ts">
  interface Props {
    size?: number;
    /** Render a flat single-color silhouette (used for the tray template
     *  icon and any other spot that wants the robot drawn in
     *  `currentColor`). */
    silhouette?: boolean;
  }

  let { size = 24, silhouette = false }: Props = $props();
</script>

{#if silhouette}
  <!-- Mirror of assets/robot_git_menubar.svg. Eyes and smile are punched
       out via a mask so a single fill color (currentColor) carries the
       whole shape — required for the macOS tray template icon. -->
  <svg width={size} height={size} viewBox="0 0 22 22" aria-hidden="true">
    <defs>
      <mask id="buddy-face-mask">
        <rect x="0" y="0" width="22" height="22" fill="#ffffff" />
        <ellipse cx="8.5" cy="13.8" rx="0.9" ry="1.4" fill="#000000" />
        <ellipse cx="13.5" cy="13.8" rx="0.9" ry="1.4" fill="#000000" />
        <path
          d="M 9.7,16.2 Q 11,17.2 12.3,16.2"
          fill="none"
          stroke="#000000"
          stroke-width="0.8"
          stroke-linecap="round"
        />
      </mask>
    </defs>
    <g fill="currentColor">
      <path
        d="M 11,10 L 11,7.8 Q 11,6.5 9.4,6.0"
        fill="none"
        stroke="currentColor"
        stroke-width="1.2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
      <path
        d="M 11,7.8 Q 12.3,7.8 13.0,6.6"
        fill="none"
        stroke="currentColor"
        stroke-width="1.2"
        stroke-linecap="round"
        stroke-linejoin="round"
      />
      <circle cx="9.2" cy="5.0" r="1.1" />
      <circle cx="13.2" cy="5.4" r="1.1" />
      <rect
        x="5"
        y="10"
        width="12"
        height="9"
        rx="2.5"
        ry="2.5"
        mask="url(#buddy-face-mask)"
      />
    </g>
  </svg>
{:else}
  <!-- Mirror of assets/robot_git_icon_sketch.svg. The squircle background
       carries the brand identity and lets the white head stay readable
       on the cream UI as well as on a dark macOS dock. -->
  <svg width={size} height={size} viewBox="0 0 512 512" aria-hidden="true">
    <defs>
      <linearGradient id="buddy-bg-grad" x1="0%" y1="0%" x2="0%" y2="100%">
        <stop offset="0%" stop-color="#efa57e" />
        <stop offset="100%" stop-color="#d46134" />
      </linearGradient>
    </defs>
    <rect x="24" y="24" width="464" height="464" rx="135" ry="135" fill="url(#buddy-bg-grad)" />
    <path
      d="M 256,155 L 256,120 Q 256,102 224,96"
      fill="none"
      stroke="#ffffff"
      stroke-width="24"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
    <path
      d="M 256,120 Q 278,120 292,103"
      fill="none"
      stroke="#ffffff"
      stroke-width="24"
      stroke-linecap="round"
      stroke-linejoin="round"
    />
    <circle cx="220" cy="82" r="20" fill="#ffffff" />
    <circle cx="296" cy="88" r="20" fill="#ffffff" />
    <rect x="116" y="150" width="280" height="236" rx="68" ry="68" fill="#ffffff" />
    <ellipse cx="196" cy="262" rx="20" ry="30" fill="#d46134" />
    <ellipse cx="316" cy="262" rx="20" ry="30" fill="#d46134" />
    <circle cx="156" cy="308" r="14" fill="#f2bea8" />
    <circle cx="356" cy="308" r="14" fill="#f2bea8" />
    <path d="M 234,306 Q 256,323 278,306" fill="none" stroke="#d46134" stroke-width="12" stroke-linecap="round" />
  </svg>
{/if}
