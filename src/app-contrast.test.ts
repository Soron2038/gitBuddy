import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { parseAppCss, parseVariants, evaluate } from '../scripts/check-contrast.mjs';

// The palette's numbers are computed, not eyeballed — this keeps them that
// way. Any token change in app.css that drops a pair below its floor fails
// `npm run test`; `node scripts/check-contrast.mjs` prints the full table.
const css = readFileSync(fileURLToPath(new URL('./app.css', import.meta.url)), 'utf8');
const { light, dark } = parseAppCss(css);

describe.each([
  ['light', light],
  ['dark', dark],
] as const)('app.css %s tokens', (scheme, tokens) => {
  const results = evaluate(tokens, scheme);

  it('resolve every token the pairs name', () => {
    expect(results.filter((r) => r.error).map((r) => r.error)).toEqual([]);
  });

  it('clear every contrast floor', () => {
    const misses = results
      .filter((r) => !r.pass)
      .map((r) => `--${r.pair.fg} on --${r.pair.bg}: ${r.ratio.toFixed(2)} < ${r.pair.floor}`);
    expect(misses).toEqual([]);
  });
});

describe('contrast maths', () => {
  it('matches the WCAG reference points', async () => {
    const { contrast, parseColor } = await import('../scripts/check-contrast.mjs');
    expect(contrast(parseColor('#000'), parseColor('#fff'))).toBeCloseTo(21, 5);
    expect(contrast(parseColor('#777'), parseColor('#fff'))).toBeCloseTo(4.48, 2);
    // A translucent foreground is composited over the background first.
    expect(contrast(parseColor('rgba(0,0,0,0)'), parseColor('#fff'))).toBeCloseTo(1, 5);
  });

  it('merges repeated palette blocks in a mockup instead of dropping the tokens', () => {
    const html = `<style>
      [data-palette="x"] { --scheme: light; --paper: #fff; --ink: #000; }
      .card[data-palette="x"] { --edge: #ccc; }
    </style>`;
    const { x } = parseVariants(html);
    expect(x.scheme).toBe('light');
    expect(x.tokens).toMatchObject({ paper: '#fff', ink: '#000', edge: '#ccc' });
  });
});
