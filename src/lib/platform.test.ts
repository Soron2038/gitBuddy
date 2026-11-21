import { describe, it, expect } from 'vitest';
import {
  detectPlatform,
  stringsFor,
  MACOS_STRINGS,
  WINDOWS_STRINGS,
  isEditableTarget,
  shouldSuppressContextMenu,
} from './platform';

// Real UA strings of the two webviews gitBuddy ships in.
const WKWEBVIEW_UA =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)';
const WEBVIEW2_UA =
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) ' +
  'Chrome/131.0.0.0 Safari/537.36 Edg/131.0.0.0';

describe('detectPlatform', () => {
  it('reads WebView2 as Windows and WKWebView as macOS', () => {
    expect(detectPlatform(WEBVIEW2_UA)).toBe('windows');
    expect(detectPlatform(WKWEBVIEW_UA)).toBe('macos');
  });

  it('falls back to macOS for anything it does not recognise', () => {
    expect(detectPlatform('')).toBe('macos');
    expect(detectPlatform(undefined)).toBe('macos');
    expect(detectPlatform(null)).toBe('macos');
    expect(detectPlatform('Node.js/22')).toBe('macos');
    // Case-sensitive on purpose: the real token is "Windows NT".
    expect(detectPlatform('windows')).toBe('macos');
  });
});

describe('platform strings', () => {
  it('keeps the macOS wording the app always used', () => {
    expect(stringsFor('macos')).toBe(MACOS_STRINGS);
    expect(MACOS_STRINGS.showInFileManager).toBe('Show in Finder');
    expect(MACOS_STRINGS.revealInFileManager).toBe('Reveal in Finder');
    expect(MACOS_STRINGS.tokenStore).toBe('your Keychain');
    expect(MACOS_STRINGS.tokenStoreLong).toBe('your macOS Keychain');
    expect(MACOS_STRINGS.mod).toBe('⌘');
    expect(MACOS_STRINGS.trayArea).toBe('menu bar');
    expect(MACOS_STRINGS.trayPopover).toBe('menu-bar popover');
    expect(MACOS_STRINGS.notificationSettings).toBe('System Settings → Notifications → gitBuddy');
    expect(MACOS_STRINGS.signIn).toBe('log in to macOS');
    expect(MACOS_STRINGS.followSystem).toBe('Follow macOS');
    expect(MACOS_STRINGS.cloneParentPlaceholder).toBe('~/Developer');
    expect(MACOS_STRINGS.terminalPlaceholder).toBe('Terminal');
  });

  it('names the Windows counterparts', () => {
    expect(stringsFor('windows')).toBe(WINDOWS_STRINGS);
    expect(WINDOWS_STRINGS.showInFileManager).toBe('Show in Explorer');
    expect(WINDOWS_STRINGS.revealInFileManager).toBe('Show in Explorer');
    expect(WINDOWS_STRINGS.tokenStore).toBe('Windows Credential Manager');
    expect(WINDOWS_STRINGS.tokenStoreLong).toBe('Windows Credential Manager');
    expect(WINDOWS_STRINGS.mod).toBe('Ctrl');
    expect(WINDOWS_STRINGS.trayArea).toBe('notification area');
    expect(WINDOWS_STRINGS.trayPopover).toBe('tray popover');
    expect(WINDOWS_STRINGS.notificationSettings).toBe(
      'Settings → System → Notifications → gitBuddy',
    );
    expect(WINDOWS_STRINGS.signIn).toBe('sign in to Windows');
    expect(WINDOWS_STRINGS.followSystem).toBe('Follow Windows');
    expect(WINDOWS_STRINGS.terminalPlaceholder).toBe('wt');
  });

  it('fills every key on both platforms, and no Windows string mentions a Mac feature', () => {
    expect(Object.keys(WINDOWS_STRINGS).sort()).toEqual(Object.keys(MACOS_STRINGS).sort());
    for (const value of Object.values(WINDOWS_STRINGS)) {
      expect(value).not.toBe('');
      expect(value).not.toMatch(/macOS|Finder|Keychain|menu.bar|⌘|System Settings/);
    }
  });
});

describe('isEditableTarget', () => {
  it('accepts text inputs, textareas and contenteditable content', () => {
    expect(isEditableTarget({ tagName: 'INPUT', type: 'text' })).toBe(true);
    expect(isEditableTarget({ tagName: 'INPUT', type: 'search' })).toBe(true);
    expect(isEditableTarget({ tagName: 'INPUT', type: 'password' })).toBe(true);
    expect(isEditableTarget({ tagName: 'input' })).toBe(true);
    expect(isEditableTarget({ tagName: 'TEXTAREA' })).toBe(true);
    expect(isEditableTarget({ tagName: 'DIV', isContentEditable: true })).toBe(true);
    expect(isEditableTarget({ tagName: 'SPAN', isContentEditable: true })).toBe(true);
  });

  it('rejects everything else', () => {
    expect(isEditableTarget(null)).toBe(false);
    expect(isEditableTarget(undefined)).toBe(false);
    expect(isEditableTarget({ tagName: 'INPUT', type: 'checkbox' })).toBe(false);
    expect(isEditableTarget({ tagName: 'INPUT', type: 'radio' })).toBe(false);
    expect(isEditableTarget({ tagName: 'INPUT', type: 'range' })).toBe(false);
    expect(isEditableTarget({ tagName: 'BUTTON' })).toBe(false);
    expect(isEditableTarget({ tagName: 'DIV', isContentEditable: false })).toBe(false);
    // SVG elements report lowercase tag names and no isContentEditable.
    expect(isEditableTarget({ tagName: 'path' })).toBe(false);
  });
});

describe('shouldSuppressContextMenu', () => {
  it('swallows the WebView2 menu on plain content', () => {
    expect(shouldSuppressContextMenu({ tagName: 'DIV' }, '')).toBe(true);
    expect(shouldSuppressContextMenu(null, '')).toBe(true);
  });

  it('keeps it for editable fields and selected text', () => {
    expect(shouldSuppressContextMenu({ tagName: 'INPUT', type: 'text' }, '')).toBe(false);
    expect(shouldSuppressContextMenu({ tagName: 'TEXTAREA' }, '')).toBe(false);
    expect(shouldSuppressContextMenu({ tagName: 'DIV' }, 'acme/api')).toBe(false);
  });
});
