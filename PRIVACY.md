# AeroFTP Privacy Policy

*Last updated: 7 March 2026*

## Overview

AeroFTP is an open source desktop application licensed under GPL-3.0. We are committed to protecting your privacy. This document explains what data AeroFTP handles and how.

## Data Collection

**AeroFTP does not collect, transmit, or store any personal data on external servers.** There is no telemetry, no analytics, no crash reporting, and no usage tracking of any kind.

## Data Stored Locally

AeroFTP stores the following data **exclusively on your local machine**:

- **Server profiles**: Hostnames, ports, usernames, and protocol settings for your configured connections. Stored in an encrypted vault database (`vault.db`) protected by AES-256-GCM with Argon2id key derivation.
- **Credentials**: Passwords, OAuth tokens, and API keys are stored in the OS keyring (GNOME Keyring, KDE Wallet, macOS Keychain, Windows Credential Manager) or in the encrypted vault database. Credentials are never stored in plaintext.
- **Application settings**: UI preferences, theme selection, language, and general configuration. Stored in the Tauri application data directory.
- **Chat history**: AI assistant conversations, if used, are stored in a local SQLite database. You can delete all chat history at any time from Settings.
- **Sync journals**: Transfer logs for the AeroSync feature. Automatically cleaned after 30 days.
- **File tags**: Color labels assigned to local files. Stored in a local SQLite database.

All local data can be deleted by removing the AeroFTP application data directory:
- **Linux**: `~/.config/aeroftp/` and `~/.local/share/aeroftp/`
- **macOS**: `~/Library/Application Support/aeroftp/`
- **Windows**: `%APPDATA%\aeroftp\`

## External Connections

AeroFTP connects to external services **only when you explicitly initiate a connection**:

### File Transfer Protocols
When you connect to a server, AeroFTP communicates directly with that server using the protocol you selected (FTP, FTPS, SFTP, WebDAV, S3, etc.). No data is routed through any intermediary.

### Cloud Storage Providers
When you connect to a cloud provider (Google Drive, Dropbox, OneDrive, etc.), AeroFTP authenticates via OAuth2 and communicates directly with the provider's API. AeroFTP does not operate any proxy or relay server.

### AI Assistant (Optional)
If you configure the AI assistant, AeroFTP sends your prompts directly to the AI provider you selected (OpenAI, Anthropic, Google, etc.) using your own API key. AeroFTP does not proxy, log, or store these requests on any external server. Your API key is stored locally in the encrypted vault.

### Auto-Update Check
AeroFTP periodically checks for updates by querying the GitHub Releases API (`api.github.com`). This is a read-only GET request that includes no personal data. You can disable this in Settings.

## Third-Party Services

AeroFTP does not integrate with any advertising, analytics, or tracking services. The only third-party connections are those you explicitly configure (file servers, cloud providers, AI providers).

## Data Sharing

We do not share, sell, or transfer any user data to third parties. AeroFTP has no server-side component — all operations happen locally or directly between your machine and the services you connect to.

## Children's Privacy

AeroFTP does not knowingly collect any data from children or any other users.

## Open Source Transparency

AeroFTP is fully open source under GPL-3.0. You can audit the complete source code at:
https://github.com/axpnet/aeroftp

## Changes to This Policy

Updates to this privacy policy will be reflected in this document with an updated date. Since AeroFTP collects no data, changes are expected to be minimal.

## Contact

For privacy-related questions or concerns:
- **Email**: aeroftp@axpdev.it
- **GitHub Issues**: https://github.com/axpnet/aeroftp/issues

---

*AeroFTP v2.8.8 — An open source project by axpdev*
