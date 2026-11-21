<script lang="ts">
  import '../app.css';
  import { isWindows, platform, shouldSuppressContextMenu } from '$lib/platform';

  interface Props {
    children?: import('svelte').Snippet;
  }
  let { children }: Props = $props();

  // Shared root of both windows (`/` and `/popover`). Set during component
  // init rather than in an effect, so the first paint already carries it.
  document.documentElement.dataset.platform = platform;

  // WebView2 answers a right-click anywhere with its own Back / Refresh /
  // Print / Inspect menu, which makes the app feel like a web page. WKWebView
  // shows nothing useful there either, but it shows little, so macOS keeps
  // its current behaviour. Rows that open gitBuddy's own menu call
  // `preventDefault` in their handler before this runs; editable fields and
  // selected text keep the native menu for Cut/Copy/Paste.
  $effect(() => {
    if (!isWindows) return;
    const onContextMenu = (e: MouseEvent) => {
      if (e.defaultPrevented) return;
      const selected = window.getSelection()?.toString() ?? '';
      if (shouldSuppressContextMenu(e.target as HTMLElement | null, selected)) {
        e.preventDefault();
      }
    };
    window.addEventListener('contextmenu', onContextMenu);
    return () => window.removeEventListener('contextmenu', onContextMenu);
  });
</script>

{@render children?.()}
