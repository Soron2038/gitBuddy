// Tauri has no Node.js server, so we run as an SPA. Disable SSR globally and
// pre-render the shell; SvelteKit's client router handles routing inside the app.
// See: https://svelte.dev/docs/kit/single-page-apps
// See: https://v2.tauri.app/start/frontend/sveltekit/
export const ssr = false;
export const prerender = true;
