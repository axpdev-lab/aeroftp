#!/usr/bin/env node

const fs = require('fs');
const path = require('path');

const ROOT = path.resolve(__dirname, '..', '..');

function read(relPath) {
  return fs.readFileSync(path.join(ROOT, relPath), 'utf8');
}

function assert(condition, message) {
  if (!condition) {
    throw new Error(message);
  }
}

function checkShellDenylist() {
  // v2.2.3: denylist moved from frontend (terminal_execute) to Rust backend (shell_execute)
  const rustFile = 'src-tauri/src/ai_tools.rs';
  const rustContent = read(rustFile);

  const requiredTokens = [
    'DENIED_COMMAND_PATTERNS',
    'pub async fn shell_execute(',
    '"Command blocked: potentially destructive system command"',
    '^\\s*rm\\s+(-[a-zA-Z]*)?.*\\s+/\\s*$',
    '^\\s*mkfs\\b',
    '^\\s*dd\\s+.*of=/dev/',
    '^\\s*shutdown\\b',
    '^\\s*reboot\\b',
    '^\\s*halt\\b',
    '^\\s*:\\(\\)\\s*\\{\\s*:\\|:\\s*&\\s*\\}\\s*;\\s*:',
  ];

  for (const token of requiredTokens) {
    assert(rustContent.includes(token), `shell denylist regression: missing token in ${rustFile}: ${token}`);
  }

  // Also verify frontend references shell_execute (not terminal_execute)
  const chatFile = 'src/components/DevTools/AIChat.tsx';
  const chatContent = read(chatFile);
  assert(chatContent.includes("toolCall.toolName === 'shell_execute'"), `shell denylist regression: AIChat.tsx should reference shell_execute`);

  // Self-test: verify regex patterns block known-bad commands
  const deniedPatterns = [
    /^\s*rm\s+(-[a-zA-Z]*)?.*\s+\/\s*$/,
    /^\s*rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+)?-[a-zA-Z]*r.*\s+\/\s*$/,
    /^\s*mkfs\b/,
    /^\s*dd\s+.*of=\/dev\//,
    /^\s*shutdown\b/,
    /^\s*reboot\b/,
    /^\s*halt\b/,
    /^\s*init\s+[06]\b/,
    /^\s*:\(\)\s*\{\s*:\|:\s*&\s*\}\s*;\s*:/,
    /^\s*>\s*\/dev\/sd[a-z]/,
    /^\s*chmod\s+(-[a-zA-Z]*\s+)?777\s+\//,
    /^\s*chown\s+.*\s+\/\s*$/,
  ];

  const knownBad = [
    'rm -rf /',
    'mkfs.ext4 /dev/sda1',
    'shutdown now',
    ':(){ :|:& };:',
  ];

  const knownSafe = [
    'ls -la',
    'pwd',
    'cat README.md',
    'npm run build',
  ];

  for (const cmd of knownBad) {
    assert(deniedPatterns.some((rx) => rx.test(cmd)), `shell denylist self-test failed: bad command not blocked: ${cmd}`);
  }

  for (const cmd of knownSafe) {
    assert(!deniedPatterns.some((rx) => rx.test(cmd)), `shell denylist self-test failed: safe command incorrectly blocked: ${cmd}`);
  }
}

function checkHostKeyFailClosed() {
  const sftpFile = 'src-tauri/src/providers/sftp.rs';
  const sshShellFile = 'src-tauri/src/ssh_shell.rs';
  const sftp = read(sftpFile);
  const sshShell = read(sshShellFile);

  const checks = [
    [sftp, sftpFile, 'Err(keys::Error::KeyChanged { line })'],
    [sftp, sftpFile, 'REJECTING connection'],
    [sftp, sftpFile, 'known_hosts verification error'],
    [sftp, sftpFile, 'Ok(false)'],
    [sshShell, sshShellFile, 'Err(keys::Error::KeyChanged { line })'],
    [sshShell, sshShellFile, 'REJECTING'],
    [sshShell, sshShellFile, 'known_hosts verification error'],
    [sshShell, sshShellFile, 'Ok(false)'],
  ];

  for (const [content, file, token] of checks) {
    assert(content.includes(token), `host-key fail-closed regression: missing token in ${file}: ${token}`);
  }
}

function checkOauthSettingsLeakGuard() {
  const settingsPanelFile = 'src/components/SettingsPanel.tsx';
  const savedServersFile = 'src/components/SavedServers.tsx';
  const settings = read(settingsPanelFile);
  const savedServers = read(savedServersFile);

  assert(
    settings.includes('localStorage.removeItem(OAUTH_SETTINGS_KEY);'),
    `oauth leak guard regression: expected legacy OAuth cleanup in ${settingsPanelFile}`
  );

  assert(
    savedServers.includes('SEC: Load credentials from vault only — no localStorage fallback.'),
    `oauth leak guard regression: missing vault-only guard comment in ${savedServersFile}`
  );

  assert(
    savedServers.includes('const loadOAuthCredentials = async (provider: string)') &&
      savedServers.includes('getCredentialWithRetry(`oauth_${provider}_client_id`)') &&
      savedServers.includes('getCredentialWithRetry(`oauth_${provider}_client_secret`)'),
    `oauth leak guard regression: expected vault/keyring OAuth credential loading path in ${savedServersFile}`
  );
}

function checkSettingsVaultMigration() {
  const settingsPanelFile = 'src/components/SettingsPanel.tsx';
  const useSettingsFile = 'src/hooks/useSettings.ts';
  const appFile = 'src/App.tsx';

  const settingsPanel = read(settingsPanelFile);
  const useSettings = read(useSettingsFile);
  const app = read(appFile);

  assert(
    settingsPanel.includes("const SETTINGS_VAULT_KEY = 'app_settings';"),
    `settings vault migration regression: missing vault key declaration in ${settingsPanelFile}`
  );

  assert(
    settingsPanel.includes('await secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, settings);'),
    `settings vault migration regression: missing vault-backed settings save in ${settingsPanelFile}`
  );

  assert(
    !settingsPanel.includes('localStorage.setItem(SETTINGS_KEY'),
    `settings vault migration regression: plaintext settings write still present in ${settingsPanelFile}`
  );

  assert(
    useSettings.includes("secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY)") &&
      useSettings.includes('secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, parsed).catch(() => {});'),
    `settings vault migration regression: missing vault-first load/migration in ${useSettingsFile}`
  );

  assert(
    app.includes("secureStoreAndClean('app_settings', SETTINGS_KEY, { ...(existing || {}), lastLocalPath: path })"),
    `settings vault migration regression: missing lastLocalPath vault write in ${appFile}`
  );
}

function checkPluginSandboxConstraints() {
  const file = 'src-tauri/src/plugins.rs';
  const content = read(file);

  const requiredTokens = [
    'SEC: Direct argv execution — no shell interpretation.',
    'Plugin command contains forbidden shell metacharacters',
    'let argv: Vec<&str> = command.split_whitespace().collect();',
    'if program.contains("..") || program.starts_with(\'/\')',
    '.current_dir(&plugin_dir)',
    '.args(&argv[1..])',
  ];

  for (const token of requiredTokens) {
    assert(content.includes(token), `plugin sandbox regression: missing token in ${file}: ${token}`);
  }
}

function run() {
  const checks = [
    ['Shell denylist', checkShellDenylist],
    ['Host-key fail-closed', checkHostKeyFailClosed],
    ['OAuth/settings leak guard', checkOauthSettingsLeakGuard],
    ['Settings vault migration', checkSettingsVaultMigration],
    ['Plugin sandbox constraints', checkPluginSandboxConstraints],
  ];

  console.log('=== Security Regression Suite (MVP) ===');
  for (const [name, fn] of checks) {
    fn();
    console.log(`✔ ${name}`);
  }
  console.log('All security regression checks passed.');
}

try {
  run();
} catch (err) {
  console.error('✖ Security regression failed');
  console.error(err instanceof Error ? err.message : String(err));
  process.exit(1);
}
