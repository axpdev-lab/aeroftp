# AeroFTP

<p align="center">
  <img src="https://github.com/axpnet/aeroftp/raw/main/icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
</p>

<p align="center">
  <strong>FTP-First. Multi-Protocol. AI-Powered. Encrypted. Privacy-Enhanced.</strong>
</p>

<p align="center">
  The modern FTP client that grew into a complete file management platform. 14 protocols, 6 integrated product modules, 47 languages, one app.
</p>

<p align="center">
  <img src="https://img.shields.io/github/v/release/axpnet/aeroftp?style=for-the-badge" alt="Latest Release">
  <img src="https://img.shields.io/github/stars/axpnet/aeroftp?style=for-the-badge" alt="GitHub Stars">
  <img src="https://img.shields.io/github/forks/axpnet/aeroftp?style=for-the-badge" alt="GitHub Forks">
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green?style=for-the-badge" alt="Platform">
  <img src="https://img.shields.io/badge/Built%20with-Tauri%202%20%2B%20React%2018-purple?style=for-the-badge" alt="Built with">
  <img src="https://img.shields.io/badge/Buy%20Me%20a%20Coffee-FFDD00?style=for-the-badge&logo=buy-me-a-coffee&logoColor=black" alt="Buy Me a Coffee">
  <img src="https://img.shields.io/badge/License-GPL--3.0-orange?style=for-the-badge" alt="License">
  <img src="https://img.shields.io/badge/Languages-47-blue?style=for-the-badge" alt="Languages">
  <img src="https://img.shields.io/badge/Protocols-14-teal?style=for-the-badge" alt="Protocols">
</p>

---

## FTP-First Design

AeroFTP is an FTP client first. Full encryption support with configurable TLS modes (Explicit AUTH TLS, Implicit TLS, opportunistic TLS), certificate verification control, MLSD/MLST machine-readable listings (RFC 3659), and resume transfers (REST/APPE). It then extends this foundation to 14 protocols and a complete file management platform through six integrated product modules — the **Aero Family**.

---

## The Aero Family

```
AeroFTP
├── AeroCloud    — Personal cloud (14 protocols, sync, share)
├── AeroFile     — Professional file manager
├── AeroSync     — Bidirectional sync engine
├── AeroVault    — Military-grade encryption
├── AeroTools    — Code editor + Terminal + AI chat
│   └── AeroAgent    — AI-powered assistant (28 tools, 10 providers)
└── AeroPlayer   — Media player with visualizers
```

---

### AeroCloud — Your Personal Cloud

Turn **any FTP server** into a private personal cloud. Connect to 14 protocols through a unified interface with bidirectional sync, tray background sync, share links, and per-project local folders.

| Protocol | Encryption | Features |
|----------|-----------|----------|
| **FTP** | None / Explicit TLS / Implicit TLS | MLSD/MLST (RFC 3659), resume transfers, TLS mode selection |
| **FTPS** | TLS/SSL (Explicit + Implicit) | Certificate verification options, self-signed cert support |
| **SFTP** | SSH | Key authentication, host key verification (TOFU), ed25519/RSA |
| **WebDAV** | HTTPS | Nextcloud, CloudMe, Koofr, Jianguoyun, InfiniCLOUD. HTTP Digest auth (RFC 2617), file locking (RFC 4918) |
| **S3** | HTTPS | AWS S3, MinIO, Backblaze B2, Wasabi, Cloudflare R2, Alibaba OSS, Tencent COS. Multipart upload |
| **Google Drive** | OAuth2 PKCE | File versions, thumbnails, share permissions, workspace export |
| **Dropbox** | OAuth2 PKCE | File versions, thumbnails, native sharing |
| **OneDrive** | OAuth2 PKCE | Resumable upload, file versions, share permissions |
| **MEGA.nz** | Client-side AES | 20GB free, end-to-end encrypted, zero-knowledge |
| **Box** | OAuth2 PKCE | 10GB free, enterprise-grade, file versions, share links |
| **pCloud** | OAuth2 | 10GB free, US/EU regions, file versions, share links |
| **Azure Blob** | HMAC-SHA256 / SAS | Enterprise blob storage, container-based, XML API |
| **4shared** | OAuth 1.0 (HMAC-SHA1) | 15GB free, native REST API, folder/file management |
| **Filen** | Client-side AES-256-GCM | 10GB free, zero-knowledge E2E encryption, PBKDF2 |

**Cloud features**: Sync index cache for faster re-scans, cross-provider remote search, storage quota display, file versions, thumbnails, share permissions, WebDAV locking, smart folder transfers with per-file conflict resolution.

**Native OS File Manager Badges** (v2.0.4+): Green checkmark on synced files, blue arrows on syncing files, red X on errors — directly inside Nautilus, Nemo, and GIO-based file managers on Linux. On Windows (v2.0.5), native Explorer sync icons via Cloud Filter API with Named Pipe IPC server. Tray icon with colored badge dots (checkmark/sync arrows/X mark overlays). One-click install on Linux, automatic on Windows.

---

### AeroFile — Professional File Manager

A full-featured local file manager built into AeroFTP. Toggle between remote and local modes, or use both side-by-side.

- **Places Sidebar**: Nautilus-style with user directories, custom locations, recent locations (with per-item delete), mounted drives with usage bars, GVFS network shares (SMB/SFTP/NFS/WebDAV), unmounted partition detection with one-click mount, EFI/swap/recovery hidden, and folder tree toggle (Ctrl+B)
- **Breadcrumb Navigation**: Clickable path segments with chevron dropdown for sibling browsing, overflow collapse, and edit mode (Ctrl+L)
- **3 View Modes**: List (detailed table), Grid (icon thumbnails), Large Icons (96px with full thumbnails). Toggle via Ctrl+1/2/3
- **Drive Detection**: Cross-platform volume detection (internal, removable, network, optical) with filesystem type, free/total space, and colored usage bars
- **Customizable Columns**: Show/hide Size, Type, Permissions, Modified columns per preference. Sort folders first and file extension visibility toggles
- **Quick Look**: Press Space to preview any file — images, video, audio, code with syntax highlighting, markdown. Arrow keys navigate between files without closing
- **Properties Dialog**: Tabbed UI with General (3 dates, symlink target), Permissions (rwx matrix, octal, owner:group), and Checksum (MD5, SHA-1, SHA-256, SHA-512)
- **Trash Browser**: Soft delete to system trash by default. Browse trash contents, restore individual files, or empty trash. Full trash lifecycle management
- **Duplicate File Finder**: Content-aware duplicate detection (size grouping + MD5 hash). Interactive dialog with KEEP/DELETE/SKIP per file and batch delete
- **Disk Usage Treemap**: Visual disk space analysis with squarified treemap algorithm. Click to drill down into directories, breadcrumb navigation, hover details
- **Batch Rename**: Find/Replace, Prefix, Suffix, Sequential numbering with live preview and conflict detection
- **Inline Rename**: Click filename or press F2 to rename directly in file list
- **File Clipboard**: Cut/Copy/Paste with cross-panel transfers (local-to-remote and vice versa)
- **Drag and Drop**: Cross-panel drag for upload/download, intra-panel drag for move
- **CompressDialog**: Unified compression UI with format selection, levels, and password protection
- **Resizable Preview Panel**: Image thumbnails, file info, dimensions, path display
- **20+ Keyboard Shortcuts**: Space preview, F2 rename, Delete, Ctrl+C/V, Ctrl+B sidebar, Ctrl+L edit path, Alt+Enter properties, and more

---

### AeroSync — Bidirectional Sync Engine

Enterprise-grade file synchronization with operational reliability features built for real-world use.

- **Smart Sync**: 3 intelligent conflict resolution modes — overwrite if newer, overwrite if different, skip if identical
- **Transfer journal with checkpoint/resume**: Persistent journal tracks every sync operation. Interrupted syncs resume from where they left off
- **SHA-256 checksum verification**: Content-based comparison with streaming 64KB-chunk hashing during scan phase
- **Post-transfer verification**: 4 policies (None, Size, Size+Time, Full) confirm transfer integrity after each download
- **Configurable retry with exponential backoff**: Per-file retry policy with base delay, max delay cap, backoff multiplier, and per-file timeout
- **Structured error taxonomy**: 10 error categories (Network, Auth, PathNotFound, PermissionDenied, QuotaExceeded, RateLimit, Timeout, FileLocked, DiskError, Unknown) with retryability hints
- **Error breakdown in sync report**: Post-sync report groups errors by category with dedicated icons
- **Navigation boundary warning**: Visual amber warning when browsing outside active sync paths
- **AeroCloud integration**: Tray background sync, share links, native OS file manager badges

---

### AeroVault — Military-Grade Encryption

Create, manage, and browse encrypted containers that protect your files with a security stack that exceeds industry standards.

**AeroVault v2 (.aerovault containers)**

| Component | Algorithm | Details |
| --------- | --------- | ------- |
| **Content encryption** | AES-256-GCM-SIV (RFC 8452) | Nonce misuse-resistant — even nonce reuse doesn't compromise security |
| **Key wrapping** | AES-256-KW (RFC 3394) | Built-in integrity check on unwrap |
| **Filename encryption** | AES-256-SIV | Deterministic — file names are hidden, not just content |
| **Key derivation** | Argon2id | 128 MiB memory / 4 iterations / 4 parallelism (exceeds OWASP 2024) |
| **Header integrity** | HMAC-SHA512 | 512-bit MAC, quantum-resistance margin |
| **Cascade mode** | ChaCha20-Poly1305 | Optional double encryption layer for defense-in-depth |
| **Chunk size** | 64 KB | Per-chunk random nonce + authentication tag |

**Additional encryption features**:
- **Directory support**: Create nested folders inside vaults with encrypted directory entries, hierarchical navigation, and recursive delete
- **Cryptomator**: Open and browse Cryptomator format 8 vaults (scrypt + AES-SIV + AES-GCM) via context menu
- **Archive Browser**: Browse ZIP, 7z, TAR, RAR contents in-app without extracting. Selective single-file extraction
- **Archive Encryption**: ZIP and 7z with AES-256 password protection. Compression levels (Store/Fast/Normal/Maximum)

---

### AeroTools — Code Editor, Terminal & AI Chat

The integrated development panel combining three tools in a tabbed interface.

- **Monaco Editor** (VS Code engine): Syntax highlighting for 50+ languages, remote file editing, 4 editor themes matching app themes
- **Integrated Terminal**: SSH remote shell with 8 terminal themes, multiple tabs, auto-sync with app theme
- **AeroAgent AI Chat**: Full AI assistant panel (see below)
- **Bidirectional sync**: Editor and AI agent edits flow in both directions in real time

#### AeroAgent — AI-Powered Assistant

An AI assistant with **28 provider-agnostic tools** that work across all 14 protocols. 10 AI providers, vision support, RAG indexing, and a plugin system.

**Providers**: OpenAI, Anthropic, Google Gemini, xAI Grok, OpenRouter, Ollama, Kimi (Moonshot), Qwen (Alibaba), DeepSeek, Custom

**Core capabilities**:
- **File operations**: List, read, search, create, edit, rename, delete — local and remote
- **Batch transfers**: Multi-file upload/download with sync preview
- **Find and replace**: Edit text in local and remote files directly from chat
- **Native function calling**: OpenAI tools[], Anthropic tool_use, Gemini functionDeclarations
- **Streaming responses**: Real-time incremental rendering for all providers
- **Context-aware**: Knows your connected server, current paths, selected files, and protocol
- **Vision/Multimodal**: Attach images for analysis — GPT-4o, Claude, Gemini, Ollama llava

**Advanced features**:
- **RAG integration**: Auto-indexes workspace files; full-text search across your codebase
- **Plugin system**: Extend with custom tools via JSON manifest + shell scripts (sandboxed, 30s timeout)
- **Multi-step autonomous tools**: Chains multiple tool calls with auto-resume after approval
- **Ollama auto-detection**: Discovers local Ollama instances and available models
- **Monaco bidirectional sync**: Live two-way sync between code editor and AI agent
- **Terminal command execution**: Run terminal commands from chat with user approval
- **Conversation export**: Export chat as Markdown or JSON
- **Prompt template library**: 15 built-in templates with `/` prefix activation
- **Streaming markdown**: Real-time rendered markdown with code block actions (Copy/Apply/Diff/Run)
- **Thinking visualization**: See AI reasoning process with token count and duration
- **Cost budget tracking**: Per-provider monthly limits with conversation-level cost display
- **Chat search**: Ctrl+F overlay with role filter and keyboard navigation

---

### AeroPlayer — Media Engine

Built-in media player with GPU-accelerated visualizations and professional audio processing.

- **14 visualizer modes**: 8 Canvas 2D + 6 WebGL 2 GPU shader modes (Wave Glitch, VHS, Mandelbrot, Raymarch Tunnel, Metaball, Particles)
- **10-band graphic EQ**: Real Web Audio BiquadFilterNode per band (32Hz-16kHz) with 10 presets and stereo balance
- **Beat detection**: Onset energy algorithm driving beat-reactive effects across all modes
- **WebGL shader engine**: 6 GLSL fragment shaders — GPU-accelerated ray marching, metaballs, fractals, particles
- **Post-processing**: Vignette, chromatic aberration, CRT scanlines, glitch effects
- **Resilient startup buffering**: First Play now queues during prebuffer and auto-starts when ready
- **Zero dependencies**: Native HTML5 `<audio>` + Web Audio API

---

## Privacy-Enhanced

AeroFTP incorporates privacy protections that go beyond what traditional file managers offer.

| Feature | Details |
| ------- | ------- |
| **Master Password** | Optional Argon2id vault encryption — all credentials locked behind a single password |
| **Encrypted Vault** | All sensitive data in AES-256-GCM encrypted storage — zero plaintext on disk |
| **Zero Telemetry** | No analytics, no phone-home, no network requests beyond user-initiated connections |
| **Memory Zeroization** | Passwords and keys cleared from RAM immediately after use |
| **Clearable History** | One-click clear for recent locations. No persistent browsing traces |
| **Portable Deployment** | AppImage runs without installation. Remove it and the config directory — no traces remain |

See [SECURITY.md](SECURITY.md) for the complete security architecture and privacy comparison, and [docs/security-evidence/README.md](docs/security-evidence/README.md) for release-by-release security evidence.

---

## Additional Features

### 4 Themes
Light, Dark, Tokyo Night, and Cyberpunk — with themed icons, terminal colors, Monaco editor syntax, and CSS custom properties throughout.

### Security Toolkit (Cyberpunk theme)
Hash Forge (MD5, SHA-1, SHA-256, SHA-512, BLAKE3), CryptoLab (AES-256-GCM, ChaCha20-Poly1305 encrypt/decrypt), Password Forge (CSPRNG + BIP39 passphrase generator with entropy display).

### 47 Languages at 100% Coverage

Quality-audited translations with native script integrity. Automatic browser language detection.

| | | | | |
|---|---|---|---|---|
| :gb: English | :it: Italian | :de: German | :es: Spanish | :fr: French |
| :portugal: Portuguese | :ru: Russian | :jp: Japanese | :kr: Korean | :cn: Chinese |
| :india: Hindi | :bangladesh: Bengali | :tr: Turkey | :poland: Polish | :netherlands: Dutch |
| :sweden: Swedish | :denmark: Danish | :norway: Norwegian | :finland: Finnish | :iceland: Icelandic |
| :czech_republic: Czech | :hungary: Hungarian | :romania: Romanian | :ukraine: Ukrainian | :greece: Greek |
| :thailand: Thai | :vietnam: Vietnamese | :indonesia: Indonesian | :malaysia: Malay | :philippines: Filipino |
| :cambodia: Khmer | :georgia: Georgian | :armenia: Armenian | :bulgaria: Bulgarian | :croatia: Croatian |
| :serbia: Serbian | :slovakia: Slovak | :slovenia: Slovenian | :macedonia: Macedonian | :estonia: Estonian |
| :lithuania: Lithuanian | :latvia: Latvian | :wales: Welsh | Catalan | Galician |
| Basque | :kenya: Swahili | | | |

### Auto-Updater
- In-app download with progress bar showing speed and ETA
- AppImage auto-install with "Install & Restart" button
- Non-intrusive update toast with auto-dismiss (StatusBar badge for persistent access)
- Periodic background check every 24 hours

---

## Installation

### Linux Snap
```bash
sudo snap install aeroftp
```

<p align="center">
  <a href="https://snapcraft.io/aeroftp">
    <img src="https://snapcraft.io/static/images/badges/en/snap-store-black.svg" alt="Get it from the Snap Store">
  </a>
</p>

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
