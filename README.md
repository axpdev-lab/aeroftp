# AeroFTP

<p align="center">
  <img src="https://github.com/axpnet/aeroftp/raw/main/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
</p>

<p align="center">
  <strong>Modern. Fast. Multi-protocol. AI-powered. Encrypted.</strong>
</p>

<p align="center">
  Cross-platform desktop client for FTP, FTPS, SFTP, WebDAV, S3-compatible storage, and cloud providers including Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob Storage, and Filen. 13 protocols, 30 presets, one app. AES-256 encrypted vaults, Cryptomator support, AI assistant with 28 tools, 10 providers, vision, RAG indexing, and plugin system. Unified encrypted keystore for all credentials. Built-in media player with 14 visualizer modes, WebGL GPU shaders, and 10-band EQ.
</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/axpnet/aeroftp?style=for-the-badge" alt="Latest Release">
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green?style=for-the-badge" alt="Platform">
  <img src="https://img.shields.io/badge/Built%20with-Tauri%202%20%2B%20React%2018-purple?style=for-the-badge" alt="Built with">
  <img src="https://img.shields.io/badge/License-GPL--3.0-orange?style=for-the-badge" alt="License">
  <img src="https://img.shields.io/badge/Languages-51-blue?style=for-the-badge" alt="Languages">
  <img src="https://img.shields.io/badge/Protocols-13-teal?style=for-the-badge" alt="Protocols">
</p>

<p align="center">
  <a href="https://snapcraft.io/aeroftp">
    <img src="https://snapcraft.io/static/images/badges/en/snap-store-black.svg" alt="Get it from the Snap Store">
  </a>
</p>

---

## Protocol Support

| Protocol | Encryption | Features |
|----------|-----------|----------|
| **FTP** | None / Explicit TLS / Implicit TLS | MLSD/MLST (RFC 3659), resume transfers, TLS mode selection |
| **FTPS** | TLS/SSL (Explicit + Implicit) | Certificate verification options, self-signed cert support |
| **SFTP** | SSH | Key authentication, host key verification (TOFU), ed25519/RSA |
| **WebDAV** | HTTPS | Nextcloud, ownCloud, Koofr, Jianguoyun, InfiniCLOUD. File locking (RFC 4918) |
| **S3** | HTTPS | AWS S3, MinIO, Backblaze B2, Wasabi, Cloudflare R2, Alibaba OSS, Tencent COS. Multipart upload |
| **Google Drive** | OAuth2 PKCE | File versions, thumbnails, share permissions, workspace export |
| **Dropbox** | OAuth2 PKCE | File versions, thumbnails, native sharing |
| **OneDrive** | OAuth2 PKCE | Resumable upload, file versions, share permissions |
| **MEGA.nz** | Client-side AES | 20GB free, end-to-end encrypted, zero-knowledge |
| **Box** | OAuth2 PKCE | 10GB free, enterprise-grade, file versions, share links |
| **pCloud** | OAuth2 | 10GB free, US/EU regions, file versions, share links |
| **Azure Blob** | HMAC-SHA256 / SAS | Enterprise blob storage, container-based, XML API |
| **Filen** | Client-side AES-256-GCM | 10GB free, zero-knowledge E2E encryption, PBKDF2 |

---

## Key Features

### FTP-First Design
AeroFTP is an FTP client first. Full encryption support with configurable TLS modes (Explicit AUTH TLS, Implicit TLS, opportunistic TLS), certificate verification control, MLSD/MLST machine-readable listings (RFC 3659), and resume transfers (REST/APPE).

### AeroCloud - Your Personal Cloud
Turn **any FTP server** into a private personal cloud with bidirectional sync, tray background sync, share links, and per-project local folders. Sync index cache enables faster re-scans and true conflict detection across all 13 protocols.

### 51 Languages
More languages than any other FTP client. RTL support for Arabic, Hebrew, Persian, and Urdu. Automatic browser language detection.

### Cloud Storage Integration
13 protocols, 30 presets in one client. Native support for Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob Storage, and Filen alongside traditional FTP/SFTP/WebDAV/S3. Cross-provider features: remote search, storage quota display in status bar, file versions, thumbnails, share permissions, and WebDAV locking.

### Universal Credential Vault (v1.8.6+)

- **Single encrypted backend**: `vault.key` + `vault.db` (AES-256-GCM) — no OS keyring dependency, works identically on all platforms
- **Auto mode** (default): CSPRNG passphrase in `vault.key` with OS file permissions. Zero user interaction on startup
- **Master mode** (optional): Passphrase encrypted with Argon2id (128 MiB, t=4, p=4). Requires master password on app start, removable from Settings
- **HKDF-SHA256**: 512-bit passphrase derived to 256-bit vault key via RFC 5869
- **Animated unlock**: Cryptographic step visualization (Argon2id derivation, decryption, HKDF expansion, integrity verification)

### Unified Encrypted Keystore (v1.9.0)

- **All credentials in vault**: Server profiles, AI API keys, OAuth tokens, and application config migrated from localStorage to AES-256-GCM encrypted `vault.db`
- **`secureStorage.ts` utility**: Vault-first storage with automatic localStorage fallback for backward compatibility
- **Migration wizard**: Auto-triggers on first launch after upgrade, migrating all legacy data to the encrypted vault
- **Keystore backup/restore**: Full vault export/import as `.aeroftp-keystore` files, protected with Argon2id (64 MB) + AES-256-GCM
- **Category tracking**: Server credentials, profiles, AI keys, OAuth tokens, and config entries tracked separately
- **Merge strategies**: Skip existing entries or overwrite all during import

### Smart Folder Transfers (v1.8.6+)

- **Folder conflict resolution**: Per-file comparison (size + date) during folder uploads/downloads — skip unchanged files automatically
- **FolderOverwriteDialog**: Merge strategies for "Ask" mode — Overwrite all, Skip identical, Overwrite if newer, Skip folder
- **Transfer Queue**: Context menu, retry failed items, remove individual items, error tooltips, header actions (Clear completed / Stop all / Clear all)

### File Clipboard (v1.8.7)

- **Cut/Copy/Paste**: Right-click context menu on files and on empty panel background
- **Cross-panel paste**: Copy from local, paste to remote (and vice versa) using stored full paths
- **Same-panel paste**: Move (cut) or copy files within the same panel, with "(copy)" suffix for duplicates

### Encryption and Vaults (v1.8.0)
- **AeroVault v2**: Military-grade encrypted containers (.aerovault) with advanced security stack:
  - *AES-256-GCM-SIV* (RFC 8452): Nonce misuse-resistant content encryption — even nonce reuse doesn't compromise security
  - *AES-256-KW* (RFC 3394): Key wrapping for master key protection with built-in integrity
  - *AES-256-SIV*: Deterministic filename encryption — file names are hidden, not just content
  - *Argon2id KDF*: 128 MiB memory / 4 iterations / 4 parallelism — exceeds OWASP 2024 high-security recommendations
  - *HMAC-SHA512*: Header integrity verification — detects tampering before decryption
  - *ChaCha20-Poly1305*: Optional cascade mode for defense-in-depth (double encryption)
  - *64 KB chunks*: Optimal streaming performance with per-chunk authentication
  - *Directory support* (v1.9.0): Create nested folders inside vaults with encrypted directory entries, hierarchical navigation with breadcrumb, and recursive delete for directories with all contents
- **Cryptomator**: Open and browse Cryptomator format 8 vaults (legacy support via context menu). Decrypt and encrypt files with scrypt + AES-SIV + AES-GCM
- **Archive Browser**: Browse contents of ZIP, 7z, TAR, and RAR archives in-app without extracting. Selective single-file extraction
- **Archive Encryption**: ZIP and 7z with AES-256 password protection. Compression levels (Store/Fast/Normal/Maximum)

### AeroAgent AI (v1.7.0+)
AI-powered assistant with **28 provider-agnostic tools** that work across all 13 protocols:
- **10 AI providers**: OpenAI, Anthropic, Google Gemini, xAI Grok, OpenRouter, Ollama, Kimi (Moonshot), Qwen (Alibaba), DeepSeek, Custom
- **RAG integration** (v1.9.0): `rag_index` auto-indexes workspace files with type/size/preview summaries; `rag_search` performs full-text search across files — AI automatically understands your codebase
- **Plugin system** (v1.9.0): Extend AeroAgent with custom tools via JSON manifest + shell scripts. Plugins run as sandboxed subprocesses with 30s timeout and 1MB output limit. 6th tab "Plugins" in AI Settings
- **Multi-step autonomous tools** (v1.9.0): Agent chains multiple tool calls to complete complex tasks. Auto-resumes after user approval of medium/high-risk tools — no need to manually prompt "continue"
- **Ollama auto-detection** (v1.9.0): Automatically discovers locally running Ollama instances and available models
- **Conversation export** (v1.9.0): Export chat history as Markdown or JSON for documentation and sharing
- **Monaco bidirectional sync** (v1.9.0): Live two-way sync between Monaco editor and AI agent — edits flow in both directions
- **Terminal command execution** (v1.9.0): AI can execute terminal commands directly from chat with user approval
- **Vision/Multimodal** (v1.8.8): Attach images for analysis — supports GPT-4o, Claude 3.5 Sonnet, Gemini Pro Vision, Ollama llava. File picker, clipboard paste, auto-resize, up to 5 images per message
- **Auto panel refresh** (v1.8.8): File panels automatically update after AI tool mutations (create, delete, rename, upload, download) — no manual refresh needed
- **Native function calling**: OpenAI tools[], Anthropic tool_use, Gemini functionDeclarations
- **Streaming responses**: Real-time incremental rendering for all providers
- **File operations**: List, read, search, create, edit, rename, delete (local + remote)
- **Batch transfers**: Multi-file upload/download, sync preview
- **Find and replace**: Edit text in local and remote files directly from chat
- **Context-aware**: Knows your connected server, current paths, selected files, and protocol
- **Compact tool approval**: Inline approval bar with danger-level color coding, expandable args, and one-click Allow/Reject
- **Collapsible messages**: Long AI responses auto-collapse with gradient fade and "Show more" toggle — keeps chat tidy
- **Chat history**: Persistent conversations with cost tracking per message

### Advanced File Management (AeroFile Pro)
- **AeroFile Mode**: Full-featured local file manager with resizable preview panel, image thumbnails, file info, and dimensions
- **Places Sidebar** (v2.0.1): Nautilus-style sidebar with user directories (Home, Documents, Downloads, etc.), custom locations, mounted drives with usage bars, and folder tree toggle (Ctrl+B)
- **Breadcrumb Navigation** (v2.0.1): Clickable path segments with chevron dropdown for sibling directory browsing, overflow collapse for deep paths, and edit mode (Ctrl+L)
- **3 View Modes** (v2.0.1): List (detailed table), Grid (icon thumbnails), and Large Icons (96px with full thumbnails for image browsing). Toggle via toolbar or Ctrl+1/2/3
- **Drive Detection** (v2.0.1): Cross-platform volume detection (internal, removable, network, optical) with filesystem type, free/total space, and colored usage bars
- **CompressDialog**: Unified compression UI with format selection, compression levels, password, and file info
- **Smart Sync** (v1.8.0): Intelligent conflict resolution — overwrite if newer, overwrite if different, skip if identical (timestamp tolerance 1s)
- **Batch Rename** (v1.8.0): Rename multiple files with Find/Replace, Add Prefix, Add Suffix, or Sequential numbering — live preview with conflict detection
- **Inline Rename** (v1.8.0): Click on filename or press F2 to rename directly in file list
- **Smart Overwrite Dialog**: File conflict resolution with comparison view
- **Properties Dialog**: Detailed metadata with checksum calculation (MD5/SHA-256)
- **Keyboard Shortcuts**: 15+ shortcuts including F2 inline rename, Delete, Ctrl+C/V, Ctrl+B sidebar, Ctrl+L edit path
- **Drag and Drop**: Cross-panel drag for upload/download, intra-panel drag for move

### AeroPlayer Media Engine (v1.9.0)
- **14 visualizer modes**: 8 Canvas 2D (bars, waveform, radial, spectrum, fractal, vortex, plasma, kaleidoscope) + 6 WebGL 2 GPU shader modes (Wave Glitch, VHS, Mandelbrot, Raymarch Tunnel, Metaball, Particles)
- **10-band graphic EQ**: Real Web Audio BiquadFilterNode per band (32Hz-16kHz) with 10 presets (Rock, Jazz, Electronic, etc.) and stereo balance control
- **Beat detection**: Onset energy algorithm with exponential decay, driving beat-reactive effects across all modes
- **WebGL shader engine**: 6 GLSL fragment shaders ported from CyberPulse — GPU-accelerated ray marching, metaball rendering, fractals, and particle systems
- **Post-processing**: Vignette, chromatic aberration, CRT scanlines, glitch effects
- **Cyber Mode**: Full cyberpunk aesthetic with Tokyo Night palette, particle system, and forced beat-triggered glitch
- **Keyboard shortcuts**: Space (play/pause), arrows (seek/volume), E (EQ), M (mute), L (loop), C (cyber), V (cycle 14 modes)
- **Zero dependencies**: Replaced Howler.js with native HTML5 `<audio>` + Web Audio API — fixes play/pause bug and large file buffering

### DevTools Panel
- **Monaco Editor** (VS Code engine) for remote file editing with syntax highlighting
- **Monaco <-> Agent bidirectional live sync** (v1.9.0): Edits in the editor and AI agent flow in both directions in real time
- Integrated **terminal** with 8 themes, multiple tabs, SSH remote shell, font size control
- **Terminal command execution from AI** (v1.9.0): AeroAgent can execute terminal commands directly with user approval
- **AeroAgent** AI assistant panel with tool approval workflow

### Security
- **Unified Encrypted Keystore** (v1.9.0): ALL sensitive data (server profiles, AI config, OAuth credentials) now stored in the AES-256-GCM encrypted vault — nothing remains in localStorage
- **Keystore backup/restore** (v1.9.0): Export/import `.aeroftp-keystore` files protected with Argon2id (64 MB) + AES-256-GCM, with category tracking and merge strategies
- **Migration wizard** (v1.9.0): Automatic one-time migration of legacy localStorage data to encrypted vault on first launch
- **AeroVault v2**: Military-grade containers with AES-256-GCM-SIV (nonce misuse-resistant), AES-256-KW key wrapping, AES-SIV filename encryption, Argon2id (128 MiB), HMAC-SHA512 header integrity, optional ChaCha20 cascade
- **Universal Vault** (v1.8.6+): Single `vault.key` + `vault.db` backend (AES-256-GCM, Argon2id, HKDF-SHA256) — no OS keyring dependency
- **Master Password** (optional): Argon2id (128 MiB, t=4, p=4) encrypted passphrase with auto-lock timeout
- **Cryptomator vaults**: Format 8 compatibility (scrypt + AES-SIV + AES-GCM) via context menu
- **XSS-hardened AI chat** (v1.8.8): Source-level HTML escaping before markdown rendering; strict CSP with no `unsafe-eval`
- **AI tool confirmation** (v1.8.8): Local filesystem tools require explicit user approval — no silent file reads
- **OAuth token protection** (v1.8.8): Tokens never written to disk unencrypted; vault auto-init or in-memory-only storage
- **SFTP host key verification**: TOFU with `~/.ssh/known_hosts`
- **Ephemeral OAuth2 port**: Random port for callbacks (no fixed port exposure)
- **FTP insecure warning**: Visual banner when using unencrypted FTP
- **Memory zeroization**: Credentials cleared via `secrecy` + `zeroize`
- **Archive passwords**: ZIP/7z passwords wrapped in SecretString, zeroed on drop

### Auto-Updater
- In-app update download with progress bar showing speed and ETA
- AppImage auto-install with "Install & Restart" button
- Periodic background check every 24 hours + tray menu manual check

---

## Installation

### Linux Snap
```bash
sudo snap install aeroftp
```
> **Note:** Snap version has limited filesystem access due to strict confinement. For full filesystem access, use .deb or .AppImage.

### Other Formats
Download from [GitHub Releases](https://github.com/axpnet/aeroftp/releases/latest):
- **Linux:** .deb, .rpm, .AppImage
- **Windows:** .exe, .msi
- **macOS:** .dmg

---

## Support the Project

AeroFTP is free and open source software. If you find it useful, please consider supporting its development:

### Donate

- **GitHub Sponsors**: [github.com/sponsors/axpnet](https://github.com/sponsors/axpnet)
- **Buy Me a Coffee**: [buymeacoffee.com/axpnet](https://buymeacoffee.com/axpnet)

### Cryptocurrency

- **Bitcoin (BTC)**: `bc1qdxur90s5j4s55rwe9rc9n95fau4rg3tfatfhkn`
- **Ethereum (ETH/EVM)**: `0x08F9D9C41E833539Fd733e19119A89f0664c3AeE`
- **Solana (SOL)**: `25A8sBNqzbR9rvrd3qyYwBkwirEh1pUiegUG6CrswHrd`
- **Litecoin (LTC)**: `LTk8iRvUqAtYyer8SPAkEAakpPXxfFY1D1`

### Contributing

Contributions are welcome. Please open an issue to discuss proposed changes before submitting a pull request.

---

## License

GPL-3.0 - See [LICENSE](LICENSE) for details.

---

*Built with Rust (Tauri 2) + React 18 + TypeScript*
