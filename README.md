# 🚀 AeroFTP

<p align="center">
  <img src="icons/AeroFTP_simbol_color_512x512.png" alt="AeroFTP Logo" width="128" height="128">
</p>

<p align="center">
  <strong>Fast. Beautiful. Reliable.</strong>
</p>

<p align="center">
  A modern, cross-platform FTP/FTPS, SFTP, and Cloud Storage client built with Rust and React.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Version-1.2.3-blue" alt="Version">
  <img src="https://img.shields.io/badge/Platform-Linux%20%7C%20Windows%20%7C%20macOS-green" alt="Platform">
  <img src="https://img.shields.io/badge/Built%20with-Tauri%202.0%20%2B%20React%2018-purple" alt="Built with">
  <img src="https://img.shields.io/badge/License-GPL--3.0-orange" alt="License">
</p>

<p align="center">
  <a href="https://snapcraft.io/aeroftp"><img src="https://snapcraft.io/static/images/badges/en/snap-store-black.svg" alt="Get it from the Snap Store"></a>
</p>

---

## ✨ Features

### 🚀 Core Features
| Feature | Description |
|---------|-------------|
| **Lightning Fast** | Built with Rust for optimal performance and memory safety |
| **Beautiful UI** | Modern design with dark/light themes and glassmorphism effects |
| **Dual Panel** | Remote and local file browsing side by side |
| **Multi-Tab Sessions** | Open multiple servers simultaneously with independent sessions |
| **FTPS & SFTP Support** | Secure FTP over TLS and SSH-based transfers |
| **Async Transfers** | Non-blocking operations with real-time progress tracking |
| **Folder Recursion** | Full recursive upload/download/delete with progress badges |

### ☁️ Multi-Provider Support (v1.2.3)
| Provider | Status | Features |
|----------|--------|----------|
| **FTP/FTPS** | ✅ Full | Browse, upload, download, sync, resume |
| **SFTP** | ✅ Full | SSH-based secure transfers |
| **Google Drive** | ✅ Full | OAuth2, browse, upload, download, **share links** |
| **Dropbox** | ✅ Full | OAuth2, browse, upload, download, **share links** |
| **OneDrive** | ✅ Full | OAuth2, browse, upload, download, **share links** |
| **WebDAV** | 🔄 Beta | Nextcloud, ownCloud, Synology compatible |
| **S3** | 🔄 Beta | AWS, MinIO, Backblaze B2, Cloudflare R2 |

### 🔗 Multi-Session OAuth Switching (NEW in v1.2.3)
Switch seamlessly between multiple cloud provider tabs without losing connection state:
- **Independent Sessions** - Each tab maintains its own OAuth connection
- **Smart Reconnection** - Automatically reconnects with correct credentials when switching
- **Clean Session Management** - Properly disconnects previous provider before connecting new one
- **StatusBar Integration** - Shows correct provider name instead of `undefined@undefined`

### 🔗 Share Links (v1.2.2)
Create public sharing links directly from the interface:
| Provider | How It Works |
|----------|-------------|
| **Google Drive** | Creates "anyone with link can view" permission |
| **Dropbox** | Uses native Sharing API |
| **OneDrive** | Creates anonymous sharing link via Microsoft Graph |
| **AeroCloud** | Uses configured `public_url_base` |

### 🔗 Navigation Sync (v0.9.9+)
| Feature | Description |
|---------|-------------|
| **Per-session sync** | Each tab maintains its own independent sync state |
| **Path coherence check** | Warning icon ⚠️ when local path doesn't match server |
| **Automatic reset** | Sync disabled by default on new connections |

### ☁️ AeroCloud
| Feature | Description |
|---------|-------------|
| **Background Sync** | Automatic file synchronization with configurable intervals |
| **Conflict Detection** | Smart handling of file conflicts with visual indicators |
| **Activity Filtering** | Toggle cloud sync messages in Activity Log |
| **Dashboard** | Visual sync status and controls |
| **Custom Names** | Set personalized display names for cloud tabs |

### 📋 Activity Log
Real-time operation tracking with dual themes:
- **Professional Theme** - Tokyo Night-inspired elegant dark theme
- **Cyber Theme** - Neon glow effects with CRT scanlines
- **Typewriter Effect** - Animated entry for new log messages
- **Humanized Messages** - Friendly, contextual messages in 5 languages
- **Badge Counter** - Shows log count in StatusBar (0 → 99+)
- **AeroCloud Filter** - Hide/show cloud sync messages

### 🛠️ DevTools Panel
Integrated developer tools:
| Tab | Feature |
|-----|---------|
| **Preview** | Syntax-highlighted file preview with syntax detection |
| **Editor** | Monaco Editor with 20+ language modes |
| **Terminal** | Local PTY terminal with GitHub Dark theme |

### 🤖 AI Assistant (AeroAgent)
| Feature | Description |
|---------|-------------|
| **Multi-Provider** | Gemini, OpenAI, Anthropic, Ollama support |
| **FTP Tools** | List, compare, sync files via natural language |
| **Smart Context** | Insert file paths with `@` mention |
| **Visual Chat** | Integrated chat interface with conversation history |

### 🌍 Internationalization
Full localization in 5 languages: **English**, **Italian**, **French**, **Spanish**, **Chinese**

---

## 📸 Screenshots

<p align="center">
  <img src="docs/screenshots/screenshot-1-2.png" alt="AeroFTP Main Interface" width="800">
</p>

<p align="center">
  <img src="docs/screenshots/screenshot-1-3.png" alt="AeroFTP Cloud and Sharing" width="800">
</p>

---

## 🛠️ Installation

### Snap Store (Linux)
```bash
sudo snap install aeroftp
