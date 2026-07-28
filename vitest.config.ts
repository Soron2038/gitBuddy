import { defineConfig } from 'vitest/config';
import { fileURLToPath } from 'node:url';

// Deliberately *not* reusing vite.config.js: the SvelteKit plugin needs
// `svelte-kit sync` to have run and pulls the whole app-shell pipeline into
// the test process, none of which these tests need. What's covered here is the
// pure logic the two windows share — key derivation, dedup, formatters,
// provider resolution — so a plain Node environment plus the `$lib` alias is
// enough, and the suite starts in milliseconds.
//
// Component tests would need the plugin and jsdom; add a second project here
// if that day comes.
export default defineConfig({
  resolve: {
    alias: {
      $lib: fileURLToPath(new URL('./src/lib', import.meta.url)),
    },
  },
  test: {
    include: ['src/**/*.test.ts'],
    environment: 'node',
  },
});
