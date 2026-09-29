#!/usr/bin/env node
// WCAG contrast check for the design tokens in src/app.css.
//
// Every colour in the palette was chosen by computing its contrast against the
// surfaces it sits on, not by eye — this script is where that computation
// lives, so a palette change re-runs the maths instead of trusting a comment.
// It reads the light `:root` block and the dark `:root` inside
// `@media (prefers-color-scheme: dark)`, resolves `var()` references, and
// checks each foreground/background pair the components actually use against
// its floor.
//
//   node scripts/check-contrast.mjs                 # src/app.css, both schemes
//   node scripts/check-contrast.mjs --html <file>   # every [data-palette] block
//                                                    # in a mockup, e.g. 04
//
// Exits 1 if any pair misses its floor. `src/app-contrast.test.ts` runs the
// same checks against src/app.css under `npm run test`.
//
// No dependencies, plain ESM + JSDoc: it has to run on the Node the repo
// already requires (>= 20), which can't execute TypeScript directly.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { resolve as resolvePath } from 'node:path';

/** @typedef {{ r: number, g: number, b: number, a: number }} Rgba */
/** @typedef {Record<string, string>} Tokens */
/** @typedef {'light' | 'dark'} Scheme */
/**
 * @typedef {object} Pair
 * @property {string} fg      Token name without the leading `--`.
 * @property {string} bg      Token name without the leading `--`.
 * @property {number} floor   Minimum contrast ratio.
 * @property {'text' | 'design' | 'ui' | 'report'} kind
 *   text: WCAG AA for body text. design: the palette's own, stricter targets.
 *   ui: WCAG 1.4.11 non-text contrast. report: printed, never fails.
 * @property {Scheme[]} [only] Restrict the pair to these schemes.
 * @property {string} [note]  Where the pair occurs, for the report.
 */
/**
 * @typedef {object} Result
 * @property {Pair} pair
 * @property {number} ratio
 * @property {boolean} pass
 * @property {string} [error]
 */

// ── Colour maths ─────────────────────────────────────────────────────────

/**
 * Parse `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb()` or `rgba()`.
 * @param {string} input
 * @returns {Rgba}
 */
export function parseColor(input) {
  const s = input.trim().toLowerCase();
  let m = s.match(/^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$/);
  if (m) {
    let hex = m[1];
    if (hex.length === 3) hex = [...hex].map((c) => c + c).join('');
    const n = (/** @type {number} */ i) => parseInt(hex.slice(i, i + 2), 16);
    return { r: n(0), g: n(2), b: n(4), a: hex.length === 8 ? n(6) / 255 : 1 };
  }
  m = s.match(/^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/);
  if (m) {
    return { r: +m[1], g: +m[2], b: +m[3], a: m[4] === undefined ? 1 : +m[4] };
  }
  if (s === 'white') return { r: 255, g: 255, b: 255, a: 1 };
  if (s === 'black') return { r: 0, g: 0, b: 0, a: 1 };
  throw new Error(`unsupported colour: ${input}`);
}

/**
 * Alpha-composite `fg` over an opaque `bg`.
 * @param {Rgba} fg
 * @param {Rgba} bg
 * @returns {Rgba}
 */
export function composite(fg, bg) {
  const mix = (/** @type {number} */ f, /** @type {number} */ b) => f * fg.a + b * (1 - fg.a);
  return { r: mix(fg.r, bg.r), g: mix(fg.g, bg.g), b: mix(fg.b, bg.b), a: 1 };
}

/**
 * WCAG 2.x relative luminance.
 * @param {Rgba} c
 */
export function luminance(c) {
  const ch = (/** @type {number} */ v) => {
    const s = v / 255;
    return s <= 0.04045 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * ch(c.r) + 0.7152 * ch(c.g) + 0.0722 * ch(c.b);
}

/**
 * WCAG contrast ratio. A translucent `fg` is composited over `bg` first; a
 * translucent `bg` is composited over `base` (the surface under it).
 * @param {Rgba} fg
 * @param {Rgba} bg
 * @param {Rgba} [base]
 */
export function contrast(fg, bg, base = { r: 255, g: 255, b: 255, a: 1 }) {
  const solidBg = bg.a < 1 ? composite(bg, base) : bg;
  const solidFg = fg.a < 1 ? composite(fg, solidBg) : fg;
  const [hi, lo] = [luminance(solidFg), luminance(solidBg)].sort((a, b) => b - a);
  return (hi + 0.05) / (lo + 0.05);
}

// ── CSS parsing ──────────────────────────────────────────────────────────

/**
 * The text between the brace that opens at or after `from` and its match.
 * @param {string} text
 * @param {number} from
 */
function braceBody(text, from) {
  const open = text.indexOf('{', from);
  if (open < 0) throw new Error('no opening brace');
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    if (text[i] === '{') depth++;
    else if (text[i] === '}' && --depth === 0) return text.slice(open + 1, i);
  }
  throw new Error('unbalanced braces');
}

/**
 * Custom-property declarations of one block (nested blocks are skipped).
 * @param {string} body
 * @returns {Tokens}
 */
export function parseDeclarations(body) {
  const flat = body.replace(/\/\*[\s\S]*?\*\//g, '');
  /** @type {Tokens} */
  const out = {};
  for (const m of flat.matchAll(/--([a-z0-9-]+)\s*:\s*([^;{}]+);/gi)) {
    out[m[1]] = m[2].trim().replace(/\s+/g, ' ');
  }
  return out;
}

/**
 * Light tokens from the first top-level `:root {}`, dark tokens from the
 * `:root {}` inside `@media (prefers-color-scheme: dark)` layered over them —
 * exactly how the cascade resolves them in the app.
 * @param {string} css
 * @returns {{ light: Tokens, dark: Tokens }}
 */
export function parseAppCss(css) {
  const text = css.replace(/\/\*[\s\S]*?\*\//g, '');
  const rootAt = text.search(/(^|\n)\s*:root\s*\{/);
  if (rootAt < 0) throw new Error('no :root block');
  const light = parseDeclarations(braceBody(text, rootAt));
  const mediaAt = text.search(/@media\s*\(\s*prefers-color-scheme\s*:\s*dark\s*\)/);
  if (mediaAt < 0) throw new Error('no dark-scheme media block');
  const media = braceBody(text, mediaAt);
  const darkRootAt = media.search(/:root\s*\{/);
  if (darkRootAt < 0) throw new Error('no :root inside the dark media block');
  return { light, dark: { ...light, ...parseDeclarations(braceBody(media, darkRootAt)) } };
}

/**
 * Palettes of a mockup page: every `[data-palette="name"] { … }` block, with
 * repeated blocks for the same name merged in order (as the cascade would).
 * A palette may declare `--scheme: light` to be checked against the light
 * floors (the default is dark).
 * @param {string} html
 * @returns {Record<string, { scheme: Scheme, tokens: Tokens }>}
 */
export function parseVariants(html) {
  const text = html.replace(/\/\*[\s\S]*?\*\//g, '');
  /** @type {Record<string, Tokens>} */
  const merged = {};
  for (const m of text.matchAll(/\[data-palette="([a-z0-9-]+)"\]\s*\{/gi)) {
    const tokens = parseDeclarations(braceBody(text, /** @type {number} */ (m.index)));
    merged[m[1]] = { ...merged[m[1]], ...tokens };
  }
  return Object.fromEntries(
    Object.entries(merged).map(([name, tokens]) => [
      name,
      { scheme: /** @type {Scheme} */ (tokens.scheme === 'light' ? 'light' : 'dark'), tokens },
    ]),
  );
}

/**
 * Resolve a token to a colour, following `var(--x)` chains.
 * @param {Tokens} tokens
 * @param {string} name
 * @returns {Rgba}
 */
export function tokenColor(tokens, name) {
  let value = tokens[name];
  for (let hops = 0; value !== undefined && hops < 8; hops++) {
    const m = value.match(/^var\(\s*--([a-z0-9-]+)\s*\)$/i);
    if (!m) return parseColor(value);
    value = tokens[m[1]];
  }
  throw new Error(`--${name} is missing or unresolvable`);
}

// ── The pairs ────────────────────────────────────────────────────────────

/** @type {Scheme[]} */
const DARK = ['dark'];

/**
 * Foreground/background pairs the components actually render. Gradient
 * surfaces are checked at both ends; the weaker end counts.
 * @type {Pair[]}
 */
export const PAIRS = [
  // Body and meta text on every surface it sits on.
  { fg: 'ink', bg: 'paper', floor: 15, kind: 'design', note: 'body text' },
  ...['paper-2', 'cream', 'cream-2', 'raised', 'head-from', 'head-to', 'titlebar-from', 'titlebar-to'].map(
    (bg) => /** @type {Pair} */ ({ fg: 'ink', bg, floor: 7, kind: 'design', note: 'headings, titles' }),
  ),
  ...['paper', 'paper-2', 'cream', 'cream-2', 'raised'].map(
    (bg) => /** @type {Pair} */ ({ fg: 'ink-2', bg, floor: 4.5, kind: 'text' }),
  ),
  ...['paper', 'paper-2'].map(
    (bg) => /** @type {Pair} */ ({ fg: 'ink-3', bg, floor: 4.5, kind: 'text', note: '11–12.5px meta' }),
  ),
  { fg: 'ink-3', bg: 'cream', floor: 4.5, kind: 'report', note: 'hover wash only' },
  { fg: 'ink-3', bg: 'cream-2', floor: 4.5, kind: 'report', note: 'meta on hover/pill' },

  // Accent text on paper.
  { fg: 'terracotta', bg: 'paper', floor: 4.5, kind: 'text', note: 'links, labels' },
  { fg: 'terracotta', bg: 'paper-2', floor: 4.5, kind: 'text', note: 'settings heading italics' },
  { fg: 'plum', bg: 'paper', floor: 4.5, kind: 'text' },
  ...['sage-ink', 'butter-ink', 'plum-ink', 'terracotta-ink'].map(
    (fg) => /** @type {Pair} */ ({ fg, bg: 'paper', floor: 4.5, kind: 'text' }),
  ),
  ...['terracotta', 'sage', 'butter', 'plum'].map(
    (fg) => /** @type {Pair} */ ({ fg, bg: 'paper', floor: 6.5, kind: 'design', only: DARK, note: 'accent on dark paper' }),
  ),

  // Each accent's ink on its own soft tint (chips, badges, counters).
  { fg: 'terracotta-ink', bg: 'terracotta-soft', floor: 4.5, kind: 'text', note: 'IS chip, NEW badge' },
  { fg: 'terracotta-ink', bg: 'raised', floor: 4.5, kind: 'text', note: 'active tab count' },
  { fg: 'sage-ink', bg: 'sage-soft', floor: 4.5, kind: 'text', note: 'PR chip' },
  { fg: 'butter-ink', bg: 'butter-soft', floor: 4.5, kind: 'text', note: 'MR chip, release chip' },
  { fg: 'plum-ink', bg: 'plum-soft', floor: 4.5, kind: 'text', note: 'pre-release badge' },
  { fg: 'ink', bg: 'terracotta-soft', floor: 7, kind: 'design', note: 'selected rows' },
  { fg: 'ink-2', bg: 'butter-soft', floor: 4.5, kind: 'text' },

  // Paper-coloured text on filled buttons and the GitHub chip.
  { fg: 'paper', bg: 'terracotta', floor: 4.5, kind: 'text', note: 'primary button' },
  { fg: 'paper', bg: 'terracotta-hover', floor: 4.5, kind: 'text', note: 'primary button hover' },
  { fg: 'paper', bg: 'ink', floor: 4.5, kind: 'text', note: 'GitHub chip, tooltip' },

  // Stat cards: small print and the big number on each tint.
  ...['t', 's', 'b'].flatMap((k) =>
    ['from', 'to'].map((end) => /** @type {Pair} */ ({ fg: 'ink-2', bg: `stat-${k}-${end}`, floor: 4.5, kind: 'text', note: 'stat label' })),
  ),
  ...[['terracotta', 't'], ['sage-ink', 's'], ['butter-ink', 'b']].flatMap(([fg, k]) =>
    // 34px display numerals: WCAG "large text", so the AA floor is 3:1.
    ['from', 'to'].map((end) => /** @type {Pair} */ ({ fg, bg: `stat-${k}-${end}`, floor: 3, kind: 'text', note: 'stat number (34px)' })),
  ),

  // Non-text: things that have to be *seen*, not read.
  { fg: 'terracotta', bg: 'paper', floor: 3, kind: 'ui', note: 'focus ring' },
  { fg: 'ink-4', bg: 'paper', floor: 3, kind: 'report', note: 'decorative dots only' },
  { fg: 'knob', bg: 'cream-3', floor: 1, kind: 'report', note: 'switch knob, off' },
  { fg: 'knob', bg: 'sage', floor: 1, kind: 'report', note: 'switch knob, on' },
  { fg: 'raised', bg: 'cream-2', floor: 1, kind: 'report', note: 'selected tab on track' },
  { fg: 'scroll-thumb', bg: 'paper', floor: 1, kind: 'report', note: 'scrollbar' },
  { fg: 'line-2', bg: 'paper', floor: 1, kind: 'report', note: 'hairline' },
];

/**
 * Check every pair against one token set.
 * @param {Tokens} tokens
 * @param {Scheme} scheme
 * @returns {Result[]}
 */
export function evaluate(tokens, scheme) {
  return PAIRS.filter((p) => !p.only || p.only.includes(scheme)).map((pair) => {
    try {
      const paper = tokenColor(tokens, 'paper');
      const ratio = contrast(tokenColor(tokens, pair.fg), tokenColor(tokens, pair.bg), paper);
      return { pair, ratio, pass: pair.kind === 'report' || ratio >= pair.floor };
    } catch (e) {
      return { pair, ratio: 0, pass: false, error: /** @type {Error} */ (e).message };
    }
  });
}

// ── CLI ──────────────────────────────────────────────────────────────────

/**
 * @param {string} title
 * @param {Result[]} results
 */
function printTable(title, results) {
  console.log(`\n${title}`);
  for (const { pair, ratio, pass, error } of results) {
    const mark = error ? '✗' : pair.kind === 'report' ? '·' : pass ? '✓' : '✗';
    const lhs = `--${pair.fg} on --${pair.bg}`.padEnd(38);
    const val = error ? error : `${ratio.toFixed(2).padStart(5)}:1`;
    const floor = pair.kind === 'report' ? '' : ` (≥ ${pair.floor})`;
    console.log(`  ${mark} ${lhs} ${val}${floor}${pair.note ? `  ${pair.note}` : ''}`);
  }
}

function main() {
  const args = process.argv.slice(2);
  const htmlAt = args.indexOf('--html');
  /** @type {Array<[string, Result[]]>} */
  const sets = [];
  if (htmlAt >= 0) {
    const file = args[htmlAt + 1];
    if (!file) throw new Error('--html needs a file');
    for (const [name, { scheme, tokens }] of Object.entries(parseVariants(readFileSync(file, 'utf8')))) {
      sets.push([`[data-palette="${name}"] (${scheme})`, evaluate(tokens, scheme)]);
    }
  } else {
    const root = resolvePath(fileURLToPath(new URL('.', import.meta.url)), '..');
    const { light, dark } = parseAppCss(readFileSync(resolvePath(root, 'src/app.css'), 'utf8'));
    sets.push(['src/app.css — light', evaluate(light, 'light')]);
    sets.push(['src/app.css — dark', evaluate(dark, 'dark')]);
  }
  let failures = 0;
  for (const [title, results] of sets) {
    printTable(title, results);
    failures += results.filter((r) => !r.pass).length;
  }
  console.log(failures ? `\n${failures} pair(s) below their floor.` : '\nAll pairs clear their floors.');
  process.exitCode = failures ? 1 : 0;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === resolvePath(process.argv[1])) main();
