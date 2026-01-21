     1	# 🚀 AeroFTP
     2	
     3	<p align="center">
     4	  <img src="icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
     5	</p>
     6	
     7	<p align="center">
     8	  <strong>Fast. Beautiful. Reliable.</strong>
     9	</p>
    10	
    11	<p align="center">
    12	  A modern, cross-platform FTP/FTPS, SFTP, and Cloud Storage client built with Rust and React.
    13	</p>
    14	
    15	<p align="center">
    16	  <img src="https://img.shields.io/badge/Version-1.2.3-blue" alt="Version">
    17	  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green" alt="Platform">
    18	  <img src="https://img.shields.io/badge/Built%20with-Tauri%202.0%20%2B%20React%2018-purple" alt="Built with">
    19	  <img src="https://img.shields.io/badge/License-GPL--3.0-orange" alt="License">
    20	</p>
    21	
    22	<p align="center">
    23	  <a href="https://snapcraft.io/aeroftp"><img src="https://snapcraft.io/static/images/badges/en/snap-store-black.svg" alt="Get it from the Snap Store"></a>
    24	</p>
    25	
    26	---
    27	
    28	## ✨ Features
    29	
    30	### 🚀 Core Features
    31	| Feature | Description |
    32	|---------|-------------|
    33	| **Lightning Fast** | Built with Rust for optimal performance and memory safety |
    34	| **Beautiful UI** | Modern design with dark/light themes and glassmorphism effects |
    35	| **Dual Panel** | Remote and local file browsing side by side |
    36	| **Multi-Tab Sessions** | Open multiple servers simultaneously with independent sessions |
    37	| **FTPS & SFTP Support** | Secure FTP over TLS and SSH-based transfers |
    38	| **Async Transfers** | Non-blocking operations with real-time progress tracking |
    39	| **Folder Recursion** | Full recursive upload/download/delete with progress badges |
    40	
    41	### ☁️ Multi-Provider Support (v1.2.3)
    42	| Provider | Status | Features |
    43	|----------|--------|----------|
    44	| **FTP/FTPS** | ✅ Full | Browse, upload, download, sync, resume |
    45	| **SFTP** | ✅ Full | SSH-based secure transfers |
    46	| **Google Drive** | ✅ Full | OAuth2, browse, upload, download, **share links** |
    47	| **Dropbox** | ✅ Full | OAuth2, browse, upload, download, **share links** |
    48	| **OneDrive** | ✅ Full | OAuth2, browse, upload, download, **share links** |
    49	| **WebDAV** | 🔄 Beta | Nextcloud, ownCloud, Synology compatible |
    50	| **S3** | 🔄 Beta | AWS, MinIO, Backblaze B2, Cloudflare R2 |
    51	
    52	### 🔗 Multi-Session OAuth Switching (NEW in v1.2.3)
    53	Switch seamlessly between multiple cloud provider tabs without losing connection state:
    54	- **Independent Sessions** - Each tab maintains its own OAuth connection
    55	- **Smart Reconnection** - Automatically reconnects with correct credentials when switching
    56	- **Clean Session Management** - Properly disconnects previous provider before connecting new one
    57	- **StatusBar Integration** - Shows correct provider name instead of `undefined@undefined`
    58	
    59	### 🔗 Share Links (v1.2.2)
    60	Create public sharing links directly from the interface:
    61	| Provider | How It Works |
    62	|----------|-------------|
    63	| **Google Drive** | Creates "anyone with link can view" permission |
    64	| **Dropbox** | Uses native Sharing API |
    65	| **OneDrive** | Creates anonymous sharing link via Microsoft Graph |
    66	| **AeroCloud** | Uses configured `public_url_base` |
    67	
    68	### 🔗 Navigation Sync (v0.9.9+)
    69	| Feature | Description |
    70	|---------|-------------|
    71	| **Per-session sync** | Each tab maintains its own independent sync state |
    72	| **Path coherence check** | Warning icon ⚠️ when local path doesn't match server |
    73	| **Automatic reset** | Sync disabled by default on new connections |
    74	
    75	### ☁️ AeroCloud
    76	| Feature | Description |
    77	|---------|-------------|
    78	| **Background Sync** | Automatic file synchronization with configurable intervals |
    79	| **Conflict Detection** | Smart handling of file conflicts with visual indicators |
    80	| **Activity Filtering** | Toggle cloud sync messages in Activity Log |
    81	| **Dashboard** | Visual sync status and controls |
    82	| **Custom Names** | Set personalized display names for cloud tabs |
    83	
    84	### 📋 Activity Log
    85	Real-time operation tracking with dual themes:
    86	- **Professional Theme** - Tokyo Night-inspired elegant dark theme
    87	- **Cyber Theme** - Neon glow effects with CRT scanlines
    88	- **Typewriter Effect** - Animated entry for new log messages
    89	- **Humanized Messages** - Friendly, contextual messages in 5 languages
    90	- **Badge Counter** - Shows log count in StatusBar (0 → 99+)
    91	- **AeroCloud Filter** - Hide/show cloud sync messages
    92	
    93	### 🛠️ DevTools Panel
    94	Integrated developer tools:
    95	| Tab | Feature |
    96	|-----|---------|
    97	| **Preview** | Syntax-highlighted file preview with syntax detection |
    98	| **Editor** | Monaco Editor with 20+ language modes |
    99	| **Terminal** | Local PTY terminal with GitHub Dark theme |
   100	
   101	### 🤖 AI Assistant (AeroAgent)
   102	| Feature | Description |
   103	|---------|-------------|
   104	| **Multi-Provider** | Gemini, OpenAI, Anthropic, Ollama support |
   105	| **FTP Tools** | List, compare, sync files via natural language |
   106	| **Smart Context** | Insert file paths with `@` mention |
   107	| **Visual Chat** | Integrated chat interface with conversation history |
   108	
   109	### 🌍 Internationalization
   110	Full localization in 5 languages: **English**, **Italian**, **French**, **Spanish**, **Chinese**
   111	
   112	---
   113	
   114	## 📸 Screenshots
   115	
   116	<p align="center">
   117	  <img src="docs/screenshots/screenshot-1.png" alt="AeroFTP Main Interface" width="800">
   118	</p>
   119	
   120	<p align="center">
   121	  <img src="docs/screenshots/screenshot-2.png" alt="AeroFTP DevTools" width="800">
   122	</p>
   123	
   124	---
   125	
   126	## 🛠️ Installation
   127	
   128	### Snap Store (Linux)
   129	```bash
   130	sudo snap install aeroftp
   131	```
   132	
   133	### From Releases
   134	Download for your platform:
   135	- **Linux**: `.deb`, `.rpm`, `.AppImage`
   136	- **Windows**: `.msi`, `.exe`
   137	- **macOS**: `.dmg`
   138	
   139	📥 [Download from GitHub Releases](https://github.com/axpnet/aeroftp/releases)
   140	
   141	### Build from Source
   142	```bash
   143	git clone https://github.com/axpnet/aeroftp.git
   144	cd aeroftp
   145	npm install
   146	npm run tauri build
   147	```
   148	
   149	**Prerequisites**: Node.js 18+, Rust 1.77+
   150	
   151	---
   152	
   153	## ⌨️ Keyboard Shortcuts
   154	
   155	| Key | Action |
   156	|-----|--------|
   157	| `Ctrl+R` | Refresh current directory |
   158	| `Ctrl+U` | Upload selected files |
   159	| `Ctrl+D` | Download selected files |
   160	| `Ctrl+N` | Create new folder |
   161	| `Delete` | Delete selected items |
   162	| `F2` | Rename selected file/folder |
   163	| `Backspace` | Navigate to parent directory |
   164	
   165	---
   166	
   167	## 🏗️ Tech Stack
   168	
   169	- **Backend**: Rust + Tauri 2.0
   170	- **Frontend**: React 18 + TypeScript
   171	- **Styling**: TailwindCSS + shadcn/ui
   172	- **FTP**: suppaftp crate
   173	- **Cloud APIs**: Google Drive API v3, Dropbox API v2, Microsoft Graph
   174	- **Editor**: Monaco Editor
   175	- **Terminal**: alacritty_terminal
   176	
   177	---
   178	
   179	## 📝 Changelog
   180	
   181	### v1.2.3 - Multi-Session OAuth Switching (21 Jan 2026)
   182	
   183	#### ✨ New Features
   184	- **Multi-Session Backend Architecture** - New `session_manager.rs` and `session_commands.rs`
   185	  - `MultiProviderState` with concurrent session management
   186	  - Session lifecycle commands: `session_connect`, `session_disconnect`, `session_switch`, `session_list`
   187	  - File operations with session context
   188	- **useSession Hook** - New React hook for multi-session provider management
   189	
   190	#### 🔧 Improvements
   191	- **OAuth Tab Switching** - Seamlessly switch between Google Drive, Dropbox, and OneDrive tabs
   192	  - Reconnects OAuth provider with correct credentials from localStorage
   193	  - Properly disconnects previous provider before connecting new one
   194	- **StatusBar Display** - Shows "Google Drive", "Dropbox", "OneDrive" instead of `undefined@undefined`
   195	- **OAuth File Operations** - mkdir, delete, rename now correctly use provider-specific commands
   196	
   197	### v1.2.2 - Share Links (20 Jan 2026)
   198	
   199	#### ✨ New Features
   200	- **Share Links for OAuth Providers** - Native share link creation for Google Drive, Dropbox, and OneDrive
   201	  - Right-click → "Create Share Link" to generate public sharing URL
   202	  - One-click copy to clipboard with toast notification
   203	- **Share Link for AeroCloud** - "Copy Share Link" in context menu (requires `public_url_base` config)
   204	
   205	#### 🔧 Improvements
   206	- OAuth folder download with recursive support
   207	- Fixed FTP connection after OAuth usage
   208	- Professional OAuth callback page with AeroFTP branding
   209	
   210	### v1.2.1 - Tab Switching Fixes (20 Jan 2026)
   211	
   212	#### 🐛 Bug Fixes
   213	- Fixed tab switching between FTP servers not updating remote file list
   214	- Fixed OAuth to FTP switch not showing connection screen
   215	- Fixed AeroCloud protocol parameter handling
   216	- Fixed "+" button not showing connection screen
   217	
   218	#### ✨ Improvements
   219	- Professional branded OAuth callback page with animations
   220	- Custom display name support for AeroCloud tabs
   221	- Translations for custom cloud name feature
   222	
   223	### v1.2.0 - Google Drive Integration (20 Jan 2026)
   224	
   225	#### ✨ Major Features
   226	- **Google Drive Support** - Full file management with OAuth2 authentication
   227	- **Multi-Provider Architecture** - Unified interface for FTP, SFTP, WebDAV, S3, and cloud providers
   228	- **Protocol Selector** - Easy switching between connection types
   229	- **Provider Tab Icons** - Visual distinction for Google Drive, Dropbox, OneDrive
   230	- **OAuth Settings Panel** - Configure API credentials in Settings → Cloud Storage
   231	
   232	### v1.1.0 - Multi-Protocol Cloud Storage
   233	
   234	#### ✨ New Features
   235	- **WebDAV Support** - Connect to Nextcloud, ownCloud, and WebDAV servers
   236	- **S3 Support** - AWS S3, MinIO, Backblaze B2, Cloudflare R2 compatible
   237	- **Protocol Selector** - Choose between FTP/FTPS/WebDAV/S3
   238	- **Unified Provider Interface** - `StorageProvider` trait for all backends
   239	
   240	### v1.0.0 - Stable Release
   241	
   242	#### 🎉 First Stable Release
   243	- Complete FTP/FTPS client with dual-panel interface
   244	- Multi-tab session management
   245	- AeroCloud sync with conflict detection
   246	- Activity Log with dual themes
   247	- DevTools panel with editor and terminal
   248	- Internationalization (5 languages)
   249	
   250	---
   251	
   252	## 🗺️ Roadmap
   253	
   254	### v1.3.0 - Client-Side Encryption (Coming Soon)
   255	- Cryptomator-compatible vault format
   256	- AES-256-GCM encryption layer
   257	- Filename encryption and obfuscation
   258	- Zero-knowledge security model
   259	
   260	### v1.4.0 - Advanced Features
   261	- Multi-cloud unified view
   262	- Cross-cloud file operations
   263	- CDN integration (CloudFront)
   264	- File versioning support
   265	- Bandwidth throttling
   266	
   267	---
   268	
   269	## 🤝 Contributing
   270	
   271	Contributions are welcome! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.
   272	
   273	### Development Setup
   274	```bash
   275	git clone https://github.com/axpnet/aeroftp.git
   276	cd aeroftp
   277	npm install
   278	npm run tauri dev  # Start development server
   279	```
   280	
   281	---
   282	
   283	## 📄 License
   284	
   285	GPL-3.0 - see [LICENSE](LICENSE) for details.
   286	
   287	---
   288	
   289	## 👥 Credits
   290	
   291	> **🤖 AI-Assisted Development**
   292	> - **Lead Developer**: [Axpdev](https://github.com/axpnet)
   293	> - **AI Assistant**: Claude (Anthropic)
   294	
   295	---
   296	
   297	<p align="center">
   298	  Made with ❤️ and ☕
   299	</p>
   300	
