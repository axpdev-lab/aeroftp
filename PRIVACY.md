# AeroFTP Privacy Policy

*Last updated: 2026-10-26*

## Overview

AeroFTP is an open source desktop application licensed under GPL-3.0. We are committed to protecting your privacy. This document explains what data AeroFTP handles and how.

## Data Collection

**AeroFTP has no product telemetry, analytics, automatic crash reporting or usage tracking.** Files and AI prompts you choose to send are transmitted to their selected recipients or services; those services have their own data policies.

## Data Stored Locally

AeroFTP stores the following data **exclusively on your local machine**:

- **Server profiles**: Hostnames, ports, usernames, and protocol settings for your configured connections. Stored in the encrypted local vault database (`vault.db`).
- **Credentials**: Passwords, OAuth tokens, and API keys are stored in the encrypted local vault database. The random passphrase that unlocks it is protected by the system keyring (default) or by a user-supplied master password in master mode. If the keyring is unavailable, AeroFTP can store that passphrase in a permission-protected `vault.key` beside the encrypted database. This fallback depends on OS file permissions; copying both files is enough to open it. On Windows, ACL hardening is best effort and grants access to the current user, SYSTEM and BUILTIN\Administrators when successful. Failures are logged without aborting vault operations; inherited access may remain or be restored. Enable a master password when filesystem permissions cannot be trusted, for cryptographic protection independent of the keyring.
- **Application settings**: UI preferences, theme selection, language, and general configuration. Stored in the Tauri application data directory.
- **Chat history**: AI assistant conversations, if used, are stored in a local SQLite database. You can delete all chat history at any time from Settings.
- **Sync journals**: Transfer logs for the AeroSync feature. You can clear them at any time, or remove entries older than 30 days, from the journal history dialog.
- **File tags**: Color labels assigned to local files. Stored in a local SQLite database.
- **Logs**: The app's diagnostic log, one file named `AeroFTP.log`, which is deleted and started again once it reaches 40 KB. It is in `%LOCALAPPDATA%\com.aeroftp.AeroFTP\logs\` on Windows, `~/.local/share/com.aeroftp.AeroFTP/logs/` on Linux (under `$XDG_DATA_HOME` when that is set) and `~/Library/Logs/com.aeroftp.AeroFTP/` on macOS. The portable Windows build keeps it in `data\logs\` inside its own folder; versions up to 4.2.4 wrote it to the Windows path above instead. The command-line tool writes no log file: what `-v` asks for goes to the terminal.

All local data can be deleted by removing the AeroFTP application data directories. After an update more than one of these can exist at the same time, because a directory named after the previous application identifier is left in place rather than moved or removed. Remove every one of them that exists, rather than choosing between them:
- **Linux**: `~/.config/aeroftp/`, `~/.local/share/app.aeroftp.AeroFTP/`, and `~/.local/share/com.aeroftp.AeroFTP/` (installations made before the identifier changed)
- **macOS**: `~/Library/Application Support/aeroftp/` and the log folder `~/Library/Logs/com.aeroftp.AeroFTP/`
- **Windows**: `%APPDATA%\aeroftp\` and `%LOCALAPPDATA%\com.aeroftp.AeroFTP\`, which holds the log and the WebView data

One thing is kept outside those directories and is not removed with them. If your vault is protected by the system keyring rather than by a master password, the key that opens it is held by the operating system's own credential store, and deleting the directories above leaves it there. AeroFTP removes it only when you switch the vault to a master password; no other action in the app deletes it.

It is stored under the service name `com.aeroftp.AeroFTP`. That name is deliberately unchanged from an earlier version of the app, so that existing vaults keep working, and it is correct even though the application identifier is now different. The account name follows how AeroFTP was installed: `vault-passphrase` for a normal installation, or `vault-passphrase-portable`, `vault-passphrase-flatpak` or `vault-passphrase-snap` for those. Development builds use the same names with a `-dev` suffix. Usually one installation stores one entry. AeroFTP never overwrites that entry, though, because the key in it may still open another vault: when it creates a vault while the entry already holds a key (after the data directory was deleted, or for a second data directory), the new vault gets an entry of its own, named like the installation's with a random suffix, for example `vault-passphrase-3f9c0a1b2d4e5f60`, and `aeroftp-cli keystore status` names the entry the current vault uses. To remove one installation's entries, remove the entry with its exact name and every entry named like it followed by a dash and 16 characters from 0-9 and a-f, as in that example. Do not go by a shorter prefix: `vault-passphrase` is also how the names of the other installation types and of the development builds begin, and removing their entries loses the key of those vaults. The commands below use the normal name; substitute the one for your installation, or the full name of a suffixed entry.

- **Linux**: in "Passwords and Keys" (Seahorse) or your desktop's keyring application, or from a terminal. `secret-tool search --all service com.aeroftp.AeroFTP` lists every AeroFTP entry with its account name (the `username` attribute); `secret-tool clear service com.aeroftp.AeroFTP username vault-passphrase` removes one entry; `secret-tool clear service com.aeroftp.AeroFTP` removes all of them, for every installation on the machine.
- **macOS**: in Keychain Access, where each entry is listed under the name `com.aeroftp.AeroFTP` with its account name, or from a terminal: `security delete-generic-password -s com.aeroftp.AeroFTP -a vault-passphrase` removes one entry; without `-a` the command removes one AeroFTP entry each time it runs, so repeat it until it reports that no item could be found.
- **Windows**: in Credential Manager, under Windows Credentials, where each entry is listed as its account name followed by `.com.aeroftp.AeroFTP`, for example `vault-passphrase.com.aeroftp.AeroFTP`.

If the vault is protected by a master password instead, nothing is kept in the keyring: the key is stored encrypted inside the data directory and is removed along with it.

## External Connections

AeroFTP connects to the services and background features you configure or enable. A fresh click on Connect is not required for every background request:

### File Transfer Protocols
When you connect to a server, AeroFTP communicates directly with that server using the protocol you selected (FTP, FTPS, SFTP, WebDAV, S3, etc.). No data is routed through any intermediary.

### Cloud Storage Providers
When you connect to a cloud provider, AeroFTP uses that provider's authentication flow and API. Depending on the provider this is OAuth, a token, a password, Proton's native browser sign-in, or an installed vendor CLI. AeroFTP does not operate a storage proxy. Enabled background sync and health checks can reconnect or probe configured services while the app is running.

### AI Assistant (Optional)
If you configure the AI assistant, AeroFTP sends your prompts directly to the AI provider you selected (OpenAI, Anthropic, Google, etc.) using your own API key. AeroFTP does not proxy, log, or store these requests on any external server. Your API key is stored locally in the encrypted vault. Tool results and file/image context included in a chat are also sent to the selected provider. Models use their configured endpoint, which may be remote even for Ollama or a custom provider. That endpoint receives the prompts, tool results and file/image context included in the request. If you enable external MCP servers, their tool calls and results pass through those servers, with per-call approval and the configured local-process or HTTP transport boundary.

### AeroShare
When enabled, AeroShare discovers peers and exchanges end-to-end encrypted data. The default discovery combines Mainline DHT, n0 discovery and LAN discovery; Settings also offers DHT-only, n0-only, LAN-only and no-discovery choices. Discovery services can observe network addresses and peer-discovery metadata. A relay can forward encrypted packets when direct connectivity is unavailable; it cannot decrypt file contents. The standing receiver is opt-in, and active shares or receiving can continue communicating in the background.

### Auto-Update Check
AeroFTP periodically checks for updates by querying the GitHub Releases API (`api.github.com`). This is a read-only GET request that includes no personal data. You can disable this in Settings.

## Third-Party Services

AeroFTP does not integrate with any advertising, analytics, or tracking services. External connections include configured file/cloud/AI/MCP services, enabled AeroShare discovery and relays, and the update-check service described above. These communications do not constitute product analytics.

## Data Sharing

AeroFTP has no product telemetry, analytics or automatic crash reporting, and we do not sell user data. Transfers, authentication, AI prompts and tool context are sent to the recipients or services you select, including configured background work. Enabled AeroShare discovery and relays can receive network metadata and encrypted traffic as described above. AeroFTP does not operate a hosted storage or file-proxy service; the services you use have their own data policies.

## Children's Privacy

AeroFTP has no product telemetry, analytics or automatic crash reporting for any age group. Files, prompts and other data sent through configured features remain subject to the selected recipients' and services' policies, including their age requirements.

## Open Source Transparency

AeroFTP is fully open source under GPL-3.0. You can audit the complete source code at:
https://github.com/axpdev-lab/aeroftp

## Changes to This Policy

Updates to this privacy policy will be reflected in this document with an updated date. Changes to features, authentication, background connections or third-party services can change the data flows described here.

## Contact

For privacy-related questions or concerns:
- **Email**: dev@aeroftp.app
- **GitHub Issues**: https://github.com/axpdev-lab/aeroftp/issues

---

*AeroFTP v4.2.5 - An open-source project by axpnet*
