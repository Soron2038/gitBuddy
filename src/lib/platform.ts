// Which OS the webview runs on, plus the handful of UI strings that name an
// OS feature (the file manager, the secret store, the shortcut modifier, …).
//
// Detection reads `navigator.userAgent` rather than pulling in
// `@tauri-apps/plugin-os`: that would be a plugin plus a capability for a
// single boolean. WKWebView reports "Macintosh", WebView2 reports
// "Windows NT 10.0; Win64; x64" — on Windows 11 too, the UA is frozen there.
// macOS is the fallback, so anything unrecognised renders exactly as before.

export type Platform = 'macos' | 'windows';

/** Pure so it can be unit-tested without a browser: only "Windows" in the
 *  UA string means Windows, everything else keeps the macOS UI. */
export function detectPlatform(userAgent: string | null | undefined): Platform {
  return userAgent?.includes('Windows') ? 'windows' : 'macos';
}

export const platform: Platform = detectPlatform(
  typeof navigator === 'undefined' ? undefined : navigator.userAgent,
);

export const isWindows = platform === 'windows';

/** Separator for a path the user hasn't typed one into yet. */
export const nativeSeparator: '/' | '\\' = isWindows ? '\\' : '/';

/** UI strings that differ per OS. Keys name the role, values are what the
 *  user reads. The macOS column is the wording the app always used — keep it
 *  byte-identical. */
export interface PlatformStrings {
  /** OS name in running text: "Fire a macOS notification …". */
  os: string;
  /** Context-menu entry / button that opens the clone folder. */
  showInFileManager: string;
  /** Tooltip on that button. */
  revealInFileManager: string;
  /** Where tokens are removed from, after "removed from". */
  tokenStore: string;
  /** Where tokens are stored, after "Tokens go into". */
  tokenStoreLong: string;
  /** Shortcut modifier shown in key hints. */
  mod: string;
  /** Where the tray icon lives: "Open the popover from the menu bar". */
  trayArea: string;
  /** The small window anchored to the tray icon. */
  trayPopover: string;
  /** How the tray icon shows the behind count, after "see which ones are
   *  behind before you start working —". */
  behindBadge: string;
  /** Path to gitBuddy's notification switch in the OS settings. */
  notificationSettings: string;
  /** Autostart help text: "Launch gitBuddy automatically when you …". */
  signIn: string;
  /** Appearance help text: "… or keep gitBuddy light or dark …". */
  followSystem: string;
  /** What the scanner skips besides `node_modules`, in the Scan roots help. */
  scanSkips: string;
  /** Placeholder for the clone form's parent directory. */
  cloneParentPlaceholder: string;
  /** Stand-in for an empty parent directory in the clone-target preview. */
  homeDir: string;
  /** Placeholder for the "Open in terminal" setting. */
  terminalPlaceholder: string;
}

export const MACOS_STRINGS: PlatformStrings = {
  os: 'macOS',
  showInFileManager: 'Show in Finder',
  revealInFileManager: 'Reveal in Finder',
  tokenStore: 'your Keychain',
  tokenStoreLong: 'your macOS Keychain',
  mod: '⌘',
  trayArea: 'menu bar',
  trayPopover: 'menu-bar popover',
  behindBadge: 'as a count on the menu-bar icon and in the popover',
  notificationSettings: 'System Settings → Notifications → gitBuddy',
  signIn: 'log in to macOS',
  followSystem: 'Follow macOS',
  scanSkips: 'build outputs and macOS junk are skipped',
  cloneParentPlaceholder: '~/Developer',
  homeDir: '~',
  terminalPlaceholder: 'Terminal',
};

export const WINDOWS_STRINGS: PlatformStrings = {
  os: 'Windows',
  showInFileManager: 'Show in Explorer',
  revealInFileManager: 'Show in Explorer',
  tokenStore: 'Windows Credential Manager',
  tokenStoreLong: 'Windows Credential Manager',
  mod: 'Ctrl',
  trayArea: 'notification area',
  trayPopover: 'tray popover',
  // The 16 px tray icon only fits a dot; the number is in its tooltip.
  behindBadge: 'as a dot on the tray icon (the count is in its tooltip) and in the popover',
  notificationSettings: 'Settings → System → Notifications → gitBuddy',
  signIn: 'sign in to Windows',
  followSystem: 'Follow Windows',
  scanSkips: 'build outputs and other heavy folders are skipped',
  cloneParentPlaceholder: 'C:\\Users\\you\\Developer',
  homeDir: '%USERPROFILE%',
  terminalPlaceholder: 'wt',
};

export function stringsFor(p: Platform): PlatformStrings {
  return p === 'windows' ? WINDOWS_STRINGS : MACOS_STRINGS;
}

/** The strings for the platform this webview runs on. */
export const strings: PlatformStrings = stringsFor(platform);

/** Input types whose right-click should keep WebView2's own Cut/Copy/Paste
 *  menu. Checkboxes, radios and sliders get nothing from it but Back,
 *  Refresh and Print. */
const TEXT_INPUT_TYPES = new Set([
  '',
  'text',
  'search',
  'url',
  'email',
  'password',
  'tel',
  'number',
]);

/** The subset of an element `isEditableTarget` reads — structural, so the
 *  tests can pass plain objects instead of needing a DOM. */
export interface EditableProbe {
  tagName: string;
  type?: string;
  isContentEditable?: boolean;
}

/** Whether a right-click on `el` should keep the native context menu: text
 *  inputs, textareas and contenteditable content (`isContentEditable` is
 *  inherited, so a child of an editable region counts too). */
export function isEditableTarget(el: EditableProbe | null | undefined): boolean {
  if (!el || typeof el.tagName !== 'string') return false;
  if (el.isContentEditable) return true;
  const tag = el.tagName.toUpperCase();
  if (tag === 'TEXTAREA') return true;
  if (tag === 'INPUT') return TEXT_INPUT_TYPES.has((el.type ?? '').toLowerCase());
  return false;
}

/** Whether to swallow WebView2's built-in context menu (Back / Refresh /
 *  Print / Inspect) for this right-click. Editable fields and selected text
 *  keep it, so Cut/Copy/Paste still work. Rows that open gitBuddy's own menu
 *  already `preventDefault` in their handler; this only covers the rest. */
export function shouldSuppressContextMenu(
  target: EditableProbe | null | undefined,
  selectedText: string,
): boolean {
  if (isEditableTarget(target)) return false;
  if (selectedText.length > 0) return false;
  return true;
}
