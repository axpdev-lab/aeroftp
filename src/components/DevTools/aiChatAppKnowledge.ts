/**
 * AeroFTP App Knowledge Base
 *
 * Structured knowledge about every AeroFTP feature, organized into 15 topic sections.
 * Used by the smart context system to inject relevant help when users ask about the app.
 *
 * Architecture:
 * - APP_KNOWLEDGE_SUMMARY (~550 tokens) is always embedded in the system prompt
 * - Individual sections (~200-400 tokens each) are injected on-demand via intent detection
 * - Budget-mode-aware: minimal=0 sections, compact=max 1, full=max 3
 */

export interface KBSection {
    id: string;
    title: string;
    keywords: string[];
    compact: string;
    full: string;
}

export const APP_KNOWLEDGE: KBSection[] = [
    // 1 — Connection Setup
    {
        id: 'connection_setup',
        title: 'Connection & Server Setup',
        keywords: [
            'connect', 'connection', 'server', 'login', 'host', 'port', 'protocol',
            'ftp', 'ftps', 'sftp', 'webdav', 's3', 'ssh', 'saved', 'bookmark',
            'add server', 'new server', 'configure', 'setup', 'endpoint', 'bucket',
            'password', 'key', 'certificate', 'tls', 'ssl', 'oauth', 'credential',
        ],
        compact: 'Connections: 14 protocols (FTP/FTPS/SFTP/WebDAV/S3/GDrive/Dropbox/OneDrive/MEGA/Box/pCloud/Azure/4shared/Filen). Configure in Connection Screen or via Saved Servers.',
        full: `How to connect to a remote server in AeroFTP:

1. **Connection Screen**: The main screen shows a connection form. Select a protocol from the dropdown (14 available).
2. **Fill in fields**: Each protocol has different required fields:
   - FTP/FTPS/SFTP: Host, Port, Username, Password (or SSH key for SFTP)
   - S3: Endpoint URL, Access Key, Secret Key, Bucket, Region
   - WebDAV: Full URL (e.g. https://cloud.example.com/remote.php/dav/files/user/)
   - Cloud providers (GDrive, Dropbox, OneDrive, Box, pCloud): Click "Connect" to start OAuth flow in browser
   - MEGA, Filen: Email + Password
   - 4shared: OAuth 1.0 (Consumer Key/Secret in Settings > Cloud Providers)
   - Azure Blob: Account Name, Access Key, Container
3. **Save Server**: Click the bookmark/save icon to store connection for later. Saved servers appear in the sidebar.
4. **Saved Servers**: Click the server list icon in the toolbar to see all saved connections. Click one to auto-fill and connect.
5. **Quick Connect**: After filling fields, click "Connect" button. Status shows in the bottom bar.
6. **SSH Keys**: For SFTP, click "Browse" next to the key field to select your OpenSSH private key file. Passphrase supported.
7. **Protocol presets**: Some providers have presets (e.g. CloudMe → WebDAV with pre-filled URL).`,
    },

    // 2 — Connection Troubleshooting
    {
        id: 'connection_troubleshoot',
        title: 'Connection Troubleshooting',
        keywords: [
            'error', 'timeout', 'refused', 'fail', 'cannot connect', 'disconnect',
            'host key', 'certificate', 'authentication', 'auth fail', 'wrong password',
            'tls', 'handshake', 'network', 'firewall', 'proxy', 'reconnect', 'port',
            'connection lost', 'broken pipe', 'econnrefused', 'econnreset',
        ],
        compact: 'Connection troubleshooting: check host/port, verify credentials, ensure TLS mode matches server, accept host keys for SFTP, check firewall/proxy settings.',
        full: `Common connection errors and solutions:

- **Connection refused**: Wrong host or port. Verify the server address. FTP=21, FTPS=990(implicit)/21(explicit), SFTP=22.
- **Authentication failed**: Wrong username/password. For SFTP, ensure your SSH key is in OpenSSH format (not PuTTY .ppk — convert with ssh-keygen).
- **TLS handshake error**: Server may not support TLS, or uses a self-signed certificate. AeroFTP shows a confirmation dialog for untrusted certs — click "Accept" to proceed.
- **Host key verification**: First SFTP connection shows fingerprint. Click "Trust" to save it. If it changes later, AeroFTP warns about possible MITM attack.
- **Timeout**: Server is unreachable. Check your network, VPN, or firewall. Try increasing timeout in Settings > General.
- **OAuth token expired**: For Google Drive, Dropbox, OneDrive, Box, pCloud — disconnect and reconnect to refresh the OAuth token.
- **MEGA 2FA**: Not yet supported. Disable 2FA temporarily or use an app-specific password.
- **Filen 2FA**: Enter the 6-digit code in the 2FA field during login.
- **S3 endpoint issues**: Use the full URL including region (e.g. https://s3.eu-west-1.amazonaws.com). Enable "Path Style" for MinIO/self-hosted.
- **Passive mode (FTP)**: Enabled by default. If behind strict NAT, ensure your firewall allows data port range.`,
    },

    // 3 — AeroCloud
    {
        id: 'aerocloud',
        title: 'AeroCloud Setup & Usage',
        keywords: [
            'aerocloud', 'cloud', 'cloud sync', 'background sync', 'auto sync',
            'cloud setup', 'cloud wizard', 'cloud dashboard', 'cloud badge',
            'cloud folder', 'sync folder', 'cloud enable', 'cloud disable',
            'cloud status', 'cloud config', 'always sync', 'real-time',
        ],
        compact: 'AeroCloud: background sync service. Setup via AeroCloud tab → 3-step wizard (name cloud, choose folder, configure). Dashboard shows sync status and history.',
        full: `AeroCloud is AeroFTP's background synchronization service that keeps a local folder in sync with a remote server.

**Setup (3-step wizard)**:
1. **Name your cloud**: Give it a descriptive name (e.g. "Work Files", "Photos Backup")
2. **Choose local folder**: Select the local directory to sync. This becomes your "cloud folder".
3. **Configure**: Set sync direction, speed mode, and schedule.

**Dashboard**:
- Shows sync status (idle, syncing, error), last sync time, files synced count
- Badge indicator shows sync state in the file manager
- Click the cloud icon in the toolbar to open dashboard

**Enable/Disable**:
- Toggle AeroCloud on/off in the dashboard header
- When disabled, no background sync occurs but configuration is preserved

**System tray**: AeroFTP runs in the system tray with a sync status badge icon. Right-click the tray icon for quick actions.

**Requirements**:
- Must have an active saved connection (the remote server to sync with)
- Local folder must exist and be writable
- AeroCloud uses the filesystem watcher (inotify on Linux) for real-time change detection

**Troubleshooting**:
- "Not syncing": Check if AeroCloud is enabled in dashboard, verify the remote connection is saved and accessible
- Inotify limit: Linux has a default watch limit. AeroFTP shows a warning if near capacity. Increase manually with sysctl.`,
    },

    // 4 — AeroSync
    {
        id: 'aerosync',
        title: 'AeroSync — File Synchronization',
        keywords: [
            'aerosync', 'sync', 'synchronize', 'synchronization', 'mirror',
            'two-way', 'backup', 'profile', 'speed mode', 'turbo', 'maniac',
            'scheduler', 'schedule', 'conflict', 'resolution', 'bandwidth',
            'compare', 'checksum', 'journal', 'resume', 'verify', 'delta',
            'template', 'rollback', 'snapshot', 'watcher', 'multi-path',
        ],
        compact: 'AeroSync: file synchronization with 3 presets (Mirror/Two-way/Backup), 5 speed modes (Normal→Maniac), scheduler, conflict resolution, templates, and journal resume.',
        full: `AeroSync is the file synchronization engine. Access via the Sync icon in the toolbar.

**Quick Sync tab**: 3 preset cards for instant sync:
- **Mirror**: One-way sync (local→remote or remote→local). Deletes orphans on target.
- **Two-way**: Bidirectional sync. Keeps newest version of each file.
- **Backup**: One-way, never deletes from target. Append-only safe backup.

**Advanced tab**: Granular control with 4 accordion sections:
- **Direction**: local→remote, remote→local, or bidirectional
- **Compare**: by size, modification time, checksum (SHA-256), or all three
- **Transfer**: retry policy (retries, backoff), verification (none/size/size+mtime/full), bandwidth limits
- **Automation**: scheduler (interval + time window with day picker), filesystem watcher

**Speed Modes** (5 levels):
- Normal (1 stream), Fast (2), Turbo (4), Extreme (8), Maniac (16 — Cyber theme only, disables safety checks)

**Conflict Resolution**:
- When both local and remote changed, AeroSync shows a conflict dialog
- Per-file: keep local, keep remote, or skip
- Batch: Keep Newer All, Keep Local All, Keep Remote All, Skip All

**Sync Journal**: Persistent journal tracks every sync operation. If interrupted, resume banner appears. Auto-cleanup after 30 days.

**Profiles**: Save custom sync configurations as named profiles.
**Templates**: Export/import sync configurations as .aerosync files for sharing or backup.
**Rollback Snapshots**: Create/delete snapshots before risky sync operations (restore coming in v2.3).
**Multi-Path**: Configure multiple local↔remote path pairs in a single sync profile.
**Watcher**: Real-time filesystem watcher with health indicator (inotify capacity warnings on Linux).`,
    },

    // 5 — AeroVault
    {
        id: 'aerovault',
        title: 'AeroVault — Encrypted Containers',
        keywords: [
            'aerovault', 'vault', 'encrypt', 'encrypted', 'encryption', 'container',
            'aes', 'security', 'lock', 'unlock', 'master password',
            'create vault', 'open vault', 'add files', 'extract', 'decrypt',
            'change password', '.aerovault', 'cryptomator',
        ],
        compact: 'AeroVault: create AES-256 encrypted .aerovault containers. Create/open via vault toolbar icon. Also supports Cryptomator vaults (format 8, read-only).',
        full: `AeroVault creates military-grade encrypted containers (.aerovault files).

**Create a vault**:
1. Click the vault icon (shield) in the toolbar → "Create New Vault"
2. Choose location and filename for the .aerovault file
3. Set a strong password (the vault uses Argon2id key derivation + AES-256-GCM-SIV encryption)

**Open a vault**:
1. Click vault icon → "Open Vault" or double-click a .aerovault file
2. Enter the password
3. VaultPanel opens showing encrypted contents

**Managing files**:
- **Add files**: Drag & drop or use the + button to add files to the vault
- **Create directory**: Click "New Folder" button, directories support nesting
- **Extract**: Select files → click Extract, or right-click → Extract
- **Delete**: Select files → Delete. Directories can be deleted recursively.
- **Navigate**: Click folders to enter them, use breadcrumb to go back

**Change password**: Open vault → Settings icon → "Change Password"

**Security info**: Settings → "Security Info" shows encryption algorithms used:
- AES-256-GCM-SIV (content), AES-256-SIV (filenames), AES-256-KW (key wrapping)
- Argon2id (KDF: 128 MiB), HMAC-SHA512 (header integrity), optional ChaCha20-Poly1305 cascade

**Cryptomator compatibility**: AeroFTP can open Cryptomator vaults (format 8, read-only). Right-click a Cryptomator vault folder → "Open as Cryptomator Vault". Supports scrypt + AES-KW + AES-SIV + AES-GCM.`,
    },

    // 6 — AeroPlayer
    {
        id: 'aeroplayer',
        title: 'AeroPlayer — Media Player',
        keywords: [
            'aeroplayer', 'player', 'music', 'audio', 'play', 'mp3', 'flac',
            'visualizer', 'equalizer', 'eq', 'beat', 'spectrum', 'waveform',
            'shader', 'webgl', 'canvas', 'volume', 'balance', 'stereo',
        ],
        compact: 'AeroPlayer: built-in audio player with 14 visualizer modes (8 Canvas + 6 WebGL), 10-band EQ with presets, beat detection, stereo balance.',
        full: `AeroPlayer is the built-in audio player. Double-click an audio file (MP3, FLAC, WAV, OGG, AAC, etc.) to play it.

**Controls**:
- Play/Pause, Previous/Next track, volume slider, progress bar with seek
- Stereo balance slider (left/right pan)

**Equalizer**:
- 10-band EQ with real BiquadFilter nodes (31Hz to 16kHz)
- 10 presets: Flat, Rock, Pop, Jazz, Classical, Bass Boost, Treble Boost, Vocal, Electronic, Acoustic
- Click the EQ icon in the player bar to open

**Visualizers** (press V to cycle):
- 8 Canvas 2D modes: Bars, Wave, Circular, Particles, Spectrum, Oscilloscope, Frequency, VU Meter
- 6 WebGL 2 GPU shaders: Wave Glitch, VHS, Mandelbrot, Raymarch Tunnel, Metaball, Particles
- Post-processing: vignette, chromatic aberration, CRT scanlines, glitch on beat
- Beat detection: automatic onset detection drives visual effects

**Tips**:
- If audio doesn't start on first play, press pause then play again (known WebKitGTK prebuffer issue)
- WebGL visualizers require GPU support
- V key cycles through all 14 modes sequentially`,
    },

    // 7 — AeroTools
    {
        id: 'aerotools',
        title: 'AeroTools — Editor, Terminal & Chat',
        keywords: [
            'aerotools', 'devtools', 'editor', 'terminal', 'code editor',
            'monaco', 'ssh', 'tab', 'panel', 'bottom panel', 'split',
            'syntax', 'highlight', 'shell', 'command line', 'pty',
        ],
        compact: 'AeroTools: bottom panel with Code Editor (Monaco), SSH Terminal (xterm.js), and AeroAgent chat. Toggle with toolbar icon or Ctrl+Shift+D.',
        full: `AeroTools is the bottom panel containing three sub-panels:

**Code Editor** (Monaco):
- Full-featured editor with syntax highlighting for 50+ languages
- Double-click a file to open it in the editor
- Auto-saves on edit with Agent→Monaco sync (live reload after AI edits)
- Context menu: "Ask AeroAgent" (Ctrl+Shift+A) sends selected code to chat
- Themes sync with app theme (Light, Dark, Tokyo Night, Cyber)

**SSH Terminal** (xterm.js):
- Full PTY terminal connected to the remote server (SFTP connections only)
- Multiple tabs supported (right-click tab → close/close others/close all)
- AeroAgent can execute shell commands via the shell_execute tool (captures stdout/stderr/exit code)
- Theme auto-syncs: Light→Solarized, Dark→GitHub Dark, Tokyo→Tokyo Night, Cyber→Neon Green

**AeroAgent Chat**:
- AI assistant with 44 tools for file management, code editing, and system tasks
- Supports 7 providers: OpenAI, Anthropic, Gemini, xAI, OpenRouter, Ollama, Custom
- Vision support: paste or attach images for analysis
- Conversation branching, export (Markdown/JSON), prompt templates (/ prefix)

**Toggle**: Click the AeroTools icon in the toolbar, or press Ctrl+Shift+D (menu shortcut).
**Resize**: Drag the top edge of the panel to resize vertically.`,
    },

    // 8 — AI Settings
    {
        id: 'ai_settings',
        title: 'AI Provider & Model Settings',
        keywords: [
            'ai settings', 'provider', 'api key', 'model', 'openai', 'anthropic',
            'claude', 'gpt', 'gemini', 'ollama', 'openrouter', 'xai', 'grok',
            'custom', 'endpoint', 'temperature', 'max tokens', 'thinking',
            'budget', 'detect', 'pull model', 'configure ai', 'setup ai',
            'kimi', 'moonshot', 'qwen', 'deepseek',
        ],
        compact: 'AI Settings: 10 providers (OpenAI, Anthropic, Gemini, xAI, OpenRouter, Ollama, Kimi, Qwen, DeepSeek, Custom). Configure API keys, add models, set temperature/tokens in Settings > AI.',
        full: `AI provider and model configuration (Settings icon in AeroAgent header → AI Settings):

**6 tabs**: Providers, Models, Advanced, System Prompt, Plugins, Macros

**Providers tab** — Configure API credentials:
- **OpenAI**: API key from platform.openai.com. Models: GPT-4o, GPT-4o-mini, o3, etc.
- **Anthropic**: API key from console.anthropic.com. Models: Claude Sonnet 4.5, Claude Opus 4.6, etc.
- **Google Gemini**: API key from ai.google.dev. Models: Gemini 2.5 Flash, Gemini 2.5 Pro, etc.
- **xAI**: API key from x.ai. Models: Grok-4, Grok-3, etc.
- **OpenRouter**: API key from openrouter.ai. Access 100+ models from all providers through one key.
- **Ollama**: Local AI. Set base URL (default: http://localhost:11434). Click "Detect" to auto-discover installed models. "Pull Model" to download new ones.
- **Kimi** (Moonshot AI): API key from platform.moonshot.cn. Chinese LLM provider.
- **Qwen** (Alibaba): API key from dashscope.aliyuncs.com. Models: Qwen-Max, Qwen-Turbo, etc.
- **DeepSeek**: API key from platform.deepseek.com. Models: DeepSeek-V3, DeepSeek-R1, etc.
- **Custom**: Any OpenAI-compatible endpoint. Set base URL + API key.

**Models tab** — Add/remove models per provider. Set context window size. Enable vision support.

**Advanced tab** — Temperature (0-2), max tokens, thinking budget (5 presets: Off/Light/Balanced/Deep/Maximum + custom slider for reasoning models like o3, Claude, Gemini).

**System Prompt tab**: Toggle and edit the system prompt. Full textarea editor.
**Plugins tab**: Manage AeroAgent plugins (shell-based extensions with JSON manifest).
**Macros tab**: View and manage composite tool macros with template variables.
**Cost budget**: Set per-provider monthly spending limits. AeroAgent tracks cumulative cost and warns when approaching the limit.`,
    },

    // 9 — AI Agent Usage
    {
        id: 'ai_agent_usage',
        title: 'Using AeroAgent',
        keywords: [
            'agent', 'aeroagent', 'chat', 'ask', 'how to use', 'tool',
            'approve', 'multi-step', 'autonomous', 'extreme', 'extreme mode', 'voice',
            'export', 'conversation', 'branch', 'template', 'prompt',
            'search chat', 'cost', 'tokens', 'shortcut', 'hotkey',
            'rag', 'memory', 'index', 'auto-approve',
        ],
        compact: 'AeroAgent: AI assistant with 44 tools for 10 AI providers. Type questions or commands. Tool calls need approval (safe=auto, medium/high=manual). Multi-step up to 10/50 steps.',
        full: `How to use AeroAgent effectively:

**Basic usage**: Type a message in the chat input and press Enter (or Ctrl+Enter). AeroAgent responds using the configured AI model.

**Tool execution**:
- AeroAgent has 44 tools for file ops, code editing, search, diff, archives, clipboard, etc.
- When a tool is needed, AeroAgent shows an approval dialog
- Safety levels: safe (auto-approved), medium (requires click), high (requires explicit confirmation)
- Batch approval: when multiple tools fire, approve/deny all at once

**Multi-step mode**:
- AeroAgent can chain up to 10 tool calls autonomously (or 50 in Extreme mode)
- Stop button appears during multi-step execution

**Extreme Mode** (Cyber theme only):
- Auto-approves ALL tool calls without asking. 50-step limit. Fully autonomous — use with caution.

**RAG Integration**: AeroAgent can index directories (rag_index) and search file contents (rag_search) for context-aware responses.
**Persistent Memory**: AeroAgent saves project notes to a .aeroagent file for cross-session recall (agent_memory_write).

**Prompt templates**: Type / in the chat input to see available templates (e.g. /analyze-ui, /performance)

**Conversation features**:
- Branch conversations (fork from any point), export as Markdown or JSON
- Search messages: Ctrl+F opens search overlay with role filter
- Cost tracking: click token count to see detailed cost breakdown

**Vision**: Paste an image (Ctrl+V) or click the image icon to attach up to 5 images for analysis.

**Keyboard shortcuts**: Ctrl+L (clear), Ctrl+Shift+N (new conversation), Ctrl+Shift+E (export), Ctrl+F (search), Ctrl+Shift+A (send editor selection to chat).`,
    },

    // 10 — File Management
    {
        id: 'file_management',
        title: 'File Management & Navigation',
        keywords: [
            'file', 'folder', 'directory', 'browse', 'navigate', 'view',
            'list', 'grid', 'icon', 'sort', 'filter', 'search', 'find',
            'quick look', 'preview', 'properties', 'permissions', 'chmod',
            'breadcrumb', 'path', 'rename', 'batch rename', 'duplicate',
            'select', 'copy', 'paste', 'cut', 'refresh', 'aerofile',
            'local mode', 'places', 'sidebar', 'hotkey',
        ],
        compact: 'File management: dual-pane (local+remote). AeroFile local-only mode. List/grid/icon views, Quick Look, Places Sidebar, batch rename, duplicate finder.',
        full: `AeroFTP provides a dual-pane file manager (local on left, remote on right):

**AeroFile mode**: Toggle remote panel off to use AeroFTP as a local-only file manager. Accessible when no server is connected.

**Views**: Toggle between List (detailed table), Grid (thumbnails), and Icon views.

**Navigation**:
- Breadcrumb bar at top — click any segment to jump
- Double-click folders to enter, ".." to go up

**Places Sidebar**: Left sidebar shows bookmarks, mounted drives, GVFS network shares (SMB/SFTP/FTP/NFS), unmounted partitions, and recent locations. Eject network shares and mount partitions directly.

**Sorting**: Click column headers (Name, Size, Date, Type, Permissions) to sort.

**File operations**:
- **Copy/Move**: Drag files between panels, or use toolbar buttons
- **Delete**: Select files → Delete key or toolbar trash icon
- **Rename**: F2 or click on a selected filename for inline rename
- **Batch Rename**: Select multiple files → right-click → Batch Rename. 4 modes: Find/Replace, Prefix, Suffix, Sequential with live preview
- **New Folder**: Ctrl+N or toolbar button
- **Properties/Permissions**: Right-click → Properties or Permissions (chmod editor)

**Quick Look**: Select a file and press Space to preview without opening (images, text, code, audio, video, PDF).

**Duplicate Finder**: Tools menu → Find Duplicates. Scans by hash to find identical files.`,
    },

    // 11 — Transfers
    {
        id: 'transfers',
        title: 'File Transfers',
        keywords: [
            'upload', 'download', 'transfer', 'drag', 'drop', 'queue',
            'progress', 'speed', 'bandwidth', 'batch', 'resume', 'retry',
            'overwrite', 'skip', 'conflict', 'drag and drop',
        ],
        compact: 'Transfers: drag & drop or toolbar buttons. Progress bar with speed graph. Queue shows all pending transfers. Overwrite dialog for conflicts.',
        full: `File transfer operations in AeroFTP:

**Upload**: Drag files from local panel to remote panel, or select files and click Upload button.
**Download**: Drag files from remote panel to local panel, or select files and click Download button.
**Drag & Drop**: Also supports dragging files from your desktop/file manager into AeroFTP.

**Progress bar**:
- Shows for each active transfer: filename, percentage, speed (MB/s), ETA
- Speed graph toggle shows real-time transfer speed chart
- Supports all 4 themes with smooth animations

**Transfer queue**:
- Multiple transfers queue automatically
- Queue panel shows pending, active, and completed transfers

**Overwrite handling**:
- When target file exists, an overwrite dialog appears
- Options: Overwrite, Skip, Rename (auto-append number), Overwrite if newer, Apply to all

**Batch transfers**: Select multiple files and transfer them all at once. AeroAgent can also trigger batch transfers via upload_files/download_files tools.

**Tips**:
- Large files use chunked transfer for reliability
- FTP uses passive mode by default for NAT compatibility
- Transfer speed depends on server and network — use AeroSync for optimized bulk transfers`,
    },

    // 12 — Archives
    {
        id: 'archives',
        title: 'Archives & Compression',
        keywords: [
            'archive', 'zip', '7z', 'tar', 'gz', 'xz', 'bz2', 'rar',
            'compress', 'extract', 'decompress', 'unzip', 'unrar',
            'browse archive', 'password', 'encryption', 'compression level',
            'create', 'make',
        ],
        compact: 'Archives: create ZIP (AES-256), 7z (AES-256), TAR/GZ/XZ/BZ2. Extract all formats including RAR. Browse archive contents without extracting.',
        full: `AeroFTP supports creating and extracting archives:

**Create archive** (right-click → Compress, or AeroAgent archive_compress tool):
- **ZIP**: Compression levels 0-9, optional AES-256 password encryption
- **7z**: LZMA2 compression, optional AES-256 encryption
- **TAR**: No encryption, combine with GZ/XZ/BZ2 for compression
- Format selection dialog shows file count, total size, and estimated compressed size

**Extract archive** (double-click or right-click → Extract):
- Supports: ZIP, 7z, TAR, GZ, XZ, BZ2, RAR (read-only)
- Password-protected archives prompt for password
- Extract to current directory or choose destination

**Browse archive** (double-click a ZIP/7z/TAR file):
- Opens archive browser showing contents as a file tree
- Preview files inside the archive without extracting
- Selective extraction: choose specific files to extract

**Compression levels**: 0 = Store (fastest), 1-3 = Fast, 4-6 = Balanced (default: 6), 7-9 = Maximum (slowest).`,
    },

    // 13 — Security
    {
        id: 'security',
        title: 'Security & Credentials',
        keywords: [
            'security', 'master password', 'keystore', 'vault backup',
            'export keystore', 'import keystore', 'migration', 'credential',
            'password manager', 'hash', 'cryptolab', 'password forge',
            'entropy', 'bip39', 'passphrase', 'cyber', 'toolkit',
        ],
        compact: 'Security: OS keyring storage (Argon2id + AES-256-GCM), vault backup/export, keystore migration wizard, Cyber security toolkit (Hash Forge, CryptoLab, Password Forge).',
        full: `Security features in AeroFTP:

**Credential storage**:
- All passwords, API keys, and tokens stored in OS keyring (libsecret on Linux, Keychain on macOS, Credential Manager on Windows)
- Encrypted with Argon2id (128 MiB) + AES-256-GCM
- Keystore migration wizard auto-triggers on first launch to migrate from legacy localStorage

**Vault Backup** (Settings → Security):
- **Export**: Saves all credentials to a .aeroftp-keystore file, encrypted with a separate password
- **Import**: Restores credentials from backup file. Merge strategies: skip existing or overwrite all

**Keystore Migration Wizard**: 4 steps: Detect → Preview → Migrate → Cleanup. Runs automatically on first launch.

**Cyber Security Toolkit** (only visible in Cyber theme):
- **Hash Forge**: Calculate MD5, SHA-1, SHA-256, SHA-512, BLAKE3 hashes for text or files. Compare two hashes. AeroAgent can also compute file hashes via the hash_file tool.
- **CryptoLab**: Encrypt/decrypt text with AES-256-GCM or ChaCha20-Poly1305.
- **Password Forge**: Generate secure passwords (CSPRNG) with length/charset options. BIP-39 passphrases up to 24 words. Entropy calculator.`,
    },

    // 14 — Settings
    {
        id: 'settings',
        title: 'App Settings & Preferences',
        keywords: [
            'settings', 'preferences', 'theme', 'light', 'dark', 'tokyo',
            'cyber', 'language', 'locale', 'startup', 'autostart',
            'icon', 'icon theme', 'general', 'appearance', 'update',
            'dark mode', 'light mode', 'mode', 'tray',
        ],
        compact: 'Settings: 4 themes (Light/Dark/Tokyo Night/Cyber), 47 languages, autostart toggle, icon themes, auto-update with in-app download. Access via gear icon.',
        full: `AeroFTP Settings (gear icon in toolbar or Ctrl+,):

**Themes** (4 available):
- **Light**: Clean white UI, professional look
- **Dark**: Dark gray UI (default), easy on the eyes
- **Tokyo Night**: Deep blue/purple palette inspired by Tokyo Night color scheme
- **Cyber**: Neon green on dark, hacker aesthetic, unlocks Security Toolkit and Extreme Mode
- Toggle: Click theme icon in toolbar header cycles through auto → light → dark → tokyo → cyber

**Language**: 47 languages available. Change in Settings → General → Language. Immediate effect, no restart needed.

**Startup**:
- Launch on system startup: toggle in Settings → General → Startup. Uses OS-native autostart (Linux .desktop, macOS LaunchAgent, Windows Registry).
- Auto-update check every 24 hours. When an update is available, AeroFTP downloads it in-app with a progress bar (%, speed, ETA). On AppImage, the update auto-installs. Manual check via Help → Check for Updates.

**System tray**: AeroFTP runs in the system tray. Tray icon shows sync status badges. Right-click for quick actions including "Check for Updates".

**Icon themes**: Choose between different file icon sets in Settings → Appearance.

**General**: Default transfer behavior, show hidden files, connection timeout, max concurrent transfers.

**AI Settings**: Separate panel accessible from AeroAgent header (see AI Settings topic for details).`,
    },

    // 15 — Troubleshooting
    {
        id: 'troubleshooting',
        title: 'General Troubleshooting',
        keywords: [
            'troubleshoot', 'not working', 'broken', 'crash', 'freeze',
            'blank', 'white screen', 'black screen', 'slow', 'performance',
            'wayland', 'webkitgtk', 'linux', 'snap', 'cache', 'clear',
            'reload', 'restart', 'reset', 'log', 'debug', 'help',
        ],
        compact: 'Troubleshooting: reload (Ctrl+R), enable debug mode (Ctrl+Shift+F12), clear cache in Settings, re-auth OAuth for expired tokens, check Wayland/WebKitGTK issues on Linux.',
        full: `General troubleshooting tips for AeroFTP:

**First steps**:
1. Reload the app: Ctrl+R (or View → Reload)
2. Enable Debug Mode: Ctrl+Shift+F12 activates debug panel with internal logs and errors
3. Restart the app completely: close and reopen

**Common issues**:
- **White/blank screen**: WebKitGTK rendering issue on Linux. Set WEBKIT_DISABLE_DMABUF_RENDERER=1 environment variable before launching.
- **Slow performance**: Close unused tabs in AeroTools. Disable visualizer in AeroPlayer. Check if filesystem watcher is overloaded.
- **OAuth not working**: Disconnect and reconnect the cloud provider to refresh tokens.
- **Theme not applying**: Switch to a different theme and back. Reload the app.
- **Snap permissions**: Snap confinement may block access to some directories. Use \`snap connect aeroftp:removable-media\` for external drives.

**Linux-specific**:
- Wayland: AeroFTP runs on XWayland by default. Environment variable WEBKIT_DISABLE_DMABUF_RENDERER=1 fixes most rendering issues.
- inotify limits: AeroCloud/watcher may fail silently if inotify watch limit is reached. Increase it with sysctl.

**Reporting bugs**: Visit the AeroFTP GitHub repository Issues page. Include: app version (Help → About), OS, steps to reproduce, Debug Panel output.

**Reset to defaults**: Delete the config directory (~/.config/aeroftp/) and restart. Warning: this removes all saved servers and settings.`,
    },
];

/**
 * Compact summary of all knowledge sections — always embedded in system prompt (~550 tokens).
 * One line per feature area for quick reference.
 */
export const APP_KNOWLEDGE_SUMMARY: string = APP_KNOWLEDGE
    .map(s => `- ${s.compact}`)
    .join('\n');
