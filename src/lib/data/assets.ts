// Release-file downloads, shared by the popover, the main window's Releases
// list and its detail pane.

import { openUrl, revealItemInDir } from '@tauri-apps/plugin-opener';
import { downloadReleaseAsset, type Release, type ReleaseAsset } from './api';
import { formatBytes } from '$lib/format';

/** Fetch a release file into ~/Downloads with the account's token and show it
 *  in Finder. When the backend can't fetch it itself — an external link, or a
 *  file the forge only serves to a signed-in browser — it answers with a URL
 *  and the browser takes over. Rejects with the backend's message. */
export async function getReleaseAsset(rel: Release, asset: ReleaseAsset): Promise<void> {
  if (!rel.account_id) {
    await openUrl(asset.browser_url);
    return;
  }
  const result = await downloadReleaseAsset(rel.account_id, rel.repo_id, rel.tag, asset.name);
  if (result.kind === 'saved') {
    await revealItemInDir(result.path);
  } else {
    await openUrl(result.url);
  }
}

/** Menu / tooltip label for one file: "app.dmg · 12 MB". */
export function assetLabel(asset: ReleaseAsset): string {
  const size = formatBytes(asset.size);
  return size ? `${asset.name} · ${size}` : asset.name;
}
