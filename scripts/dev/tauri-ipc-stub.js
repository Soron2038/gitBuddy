// Tauri IPC stub for visual verification of the frontend in a plain browser
// (vite dev server, no Tauri shell). Fakes `window.__TAURI_INTERNALS__` with
// deterministic fixture data so before/after screenshots of a refactoring
// are comparable — the real app's live data changes between shots.
//
// Usage (Playwright):
//   await page.context().addInitScript({ path: 'scripts/dev/tauri-ipc-stub.js' });
//   await page.goto('http://localhost:1420/');   // after `npm run dev`
//
// The stub must be registered BEFORE the SPA boots (init script, not
// evaluate) because both windows call invoke() from onMount. Unhandled
// commands log a console warning and resolve to null; extend RESPONSES
// below when new commands appear.
(() => {
  const NOW = new Date('2026-06-10T16:00:00Z').getTime();
  const iso = (msAgo) => new Date(NOW - msAgo).toISOString();
  const H = 3600e3, D = 24 * H;

  const ACCOUNT = {
    id: 'github:github.com:soron2038',
    provider: 'github',
    login: 'soron2038',
    viewer: { login: 'soron2038', avatar_url: null, name: 'Björn' },
    auth: 'oauth_device',
    base_url: null,
    added_at: iso(30 * D),
  };

  const REPOS = [
    {
      id: 'gh:1', owner: 'Soron2038', name: 'gitBuddy', provider: 'github',
      default_branch: 'main', language: 'Rust',
      description: 'Menu-bar companion for GitHub, GitLab and Codeberg.',
      stars: 42, html_url: 'https://github.com/Soron2038/gitBuddy',
      ssh_url: 'git@github.com:Soron2038/gitBuddy.git',
      clone_url: 'https://github.com/Soron2038/gitBuddy.git',
      is_fork: false, is_private: false, pushed_at: iso(2 * H), account_id: ACCOUNT.id,
    },
    {
      id: 'gh:2', owner: 'Soron2038', name: 'website', provider: 'github',
      default_branch: 'main', language: 'Svelte',
      description: 'Personal site — remote-only, exercises the clone form.',
      stars: 3, html_url: 'https://github.com/Soron2038/website',
      ssh_url: 'git@github.com:Soron2038/website.git',
      clone_url: 'https://github.com/Soron2038/website.git',
      is_fork: false, is_private: true, pushed_at: iso(3 * D), account_id: ACCOUNT.id,
    },
    {
      id: 'gh:3', owner: 'Soron2038', name: 'experiments', provider: 'github',
      default_branch: 'main', language: null,
      description: null,
      stars: 0, html_url: 'https://github.com/Soron2038/experiments',
      ssh_url: null, clone_url: 'https://github.com/Soron2038/experiments.git',
      is_fork: true, is_private: false, pushed_at: iso(60 * D), account_id: ACCOUNT.id,
    },
    {
      id: 'gh:4', owner: 'mpsd', name: 'analysis-tools', provider: 'github',
      default_branch: 'develop', language: 'Python',
      description: 'Shared analysis tooling with a rather long description to test row wrapping behaviour in the grid.',
      stars: 17, html_url: 'https://github.com/mpsd/analysis-tools',
      ssh_url: 'git@github.com:mpsd/analysis-tools.git',
      clone_url: 'https://github.com/mpsd/analysis-tools.git',
      is_fork: false, is_private: false, pushed_at: iso(5 * D), account_id: ACCOUNT.id,
    },
  ];

  const WAITING = [
    {
      id: 'w1', kind: 'PR', title: 'Review: parallel provider fan-out',
      repo: 'Soron2038/gitBuddy', provider: 'github', reason: 'review',
      url: 'https://github.com/Soron2038/gitBuddy/pull/12',
      age_human: '2h', updated_at: iso(2 * H), account_id: ACCOUNT.id,
    },
    {
      id: 'w2', kind: 'IS', title: 'Tray icon blurry on external display',
      repo: 'Soron2038/gitBuddy', provider: 'github', reason: 'assigned',
      url: 'https://github.com/Soron2038/gitBuddy/issues/8',
      age_human: '1d', updated_at: iso(D), account_id: ACCOUNT.id,
    },
    {
      id: 'w3', kind: 'PR', title: 'Bump dependencies for analysis pipeline',
      repo: 'mpsd/analysis-tools', provider: 'github', reason: 'mentioned',
      url: 'https://github.com/mpsd/analysis-tools/pull/3',
      age_human: '3d', updated_at: iso(3 * D), account_id: ACCOUNT.id,
    },
  ];

  const RELEASES = [
    {
      repo_id: 'gh:1', repo_full_name: 'Soron2038/gitBuddy', provider: 'github',
      tag: 'v1.0.1', name: 'gitBuddy 1.0.1', published_at: iso(5 * D),
      html_url: 'https://github.com/Soron2038/gitBuddy/releases/tag/v1.0.1',
      is_prerelease: false, is_new: true, age_human: '5d', account_id: ACCOUNT.id,
    },
    {
      repo_id: 'gh:4', repo_full_name: 'mpsd/analysis-tools', provider: 'github',
      tag: 'v0.9.0-rc1', name: 'Release candidate', published_at: iso(2 * D),
      html_url: 'https://github.com/mpsd/analysis-tools/releases/tag/v0.9.0-rc1',
      is_prerelease: true, is_new: true, age_human: '2d', account_id: ACCOUNT.id,
    },
  ];

  const CI = [
    { repo_id: 'gh:1', repo_full_name: 'Soron2038/gitBuddy', status: 'ok',
      html_url: 'https://github.com/Soron2038/gitBuddy/actions/runs/1',
      branch: 'main', workflow_name: 'CI', author_login: 'soron2038', account_id: ACCOUNT.id },
    { repo_id: 'gh:2', repo_full_name: 'Soron2038/website', status: 'fail',
      html_url: 'https://github.com/Soron2038/website/actions/runs/2',
      branch: 'main', workflow_name: 'Deploy', author_login: 'soron2038', account_id: ACCOUNT.id },
    { repo_id: 'gh:4', repo_full_name: 'mpsd/analysis-tools', status: 'run',
      html_url: null, branch: 'develop', workflow_name: 'Tests', author_login: null, account_id: ACCOUNT.id },
  ];

  const LOCALS = [
    {
      path: '/Users/witt/Developer/gitBuddy', branch: 'main',
      remote: { host: 'github.com', owner: 'Soron2038', name: 'gitBuddy',
                raw_url: 'https://github.com/Soron2038/gitBuddy.git' },
      dirty_staged: 1, dirty_unstaged: 2, untracked: 1, ahead: 2, behind: 0, detached: false,
    },
    {
      path: '/Users/witt/Developer/scratch/orphan-notes', branch: 'master',
      remote: { host: 'gitlab.gwdg.de', owner: 'witt', name: 'notes',
                raw_url: 'https://gitlab.gwdg.de/witt/notes.git' },
      dirty_staged: 0, dirty_unstaged: 0, untracked: 0, ahead: 0, behind: 3, detached: false,
    },
  ];

  const SETTINGS = {
    version: 3,
    scan_roots: ['/Users/witt/Developer'],
    scan_ignore: ['node_modules'],
    gitlab_base_url: null,
    codeberg_base_url: null,
    editor_command: 'code',
    terminal_command: 'iTerm',
    notifications: { enabled: true, do_not_disturb: false,
                     events: { waiting: true, releases: true, ci_failure: true } },
    poll_interval_minutes: 5,
  };

  const RESPONSES = {
    accounts_list: () => [ACCOUNT],
    list_waiting: () => WAITING,
    list_repos: () => REPOS,
    list_releases: () => RELEASES,
    list_ci: () => CI,
    list_local_repos: () => LOCALS,
    get_settings: () => SETTINGS,
    save_settings: () => null,
    last_sync_info: () => ({ synced_at: iso(90e3), last_error: null }),
    aggregator_refresh_now: () => null,
    run_editor: () => null,
    run_terminal: () => null,
    export_config: () => null,
    open_main: () => null,
    open_main_settings: () => null,
    'plugin:app|version': () => '1.0.1+stub',
    'plugin:autostart|is_enabled': () => false,
    'plugin:updater|check': () => null,
    'plugin:notification|is_permission_granted': () => true,
    'plugin:event|listen': () => 1,
    'plugin:event|unlisten': () => null,
    'plugin:opener|open_url': () => null,
    'plugin:clipboard-manager|write_text': () => null,
  };

  window.__TAURI_IPC_STUB_CALLS__ = [];
  // The event plugin's unlisten path reaches for this on teardown.
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = { unregisterListener: () => {} };
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { label: 'main' } },
    transformCallback: (cb) => Math.floor(Math.random() * 1e9),
    invoke: async (cmd, args) => {
      window.__TAURI_IPC_STUB_CALLS__.push(cmd);
      const handler = RESPONSES[cmd];
      if (handler) return handler(args);
      console.warn('[tauri-stub] unhandled command:', cmd, args);
      return null;
    },
  };
})();
