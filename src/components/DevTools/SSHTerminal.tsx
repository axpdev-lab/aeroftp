// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect, useRef, useState, useCallback } from 'react';
import { useTranslation } from '../../i18n';
import { Terminal as XTerm, type IDisposable } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Terminal as TerminalIcon, Play, Square, RotateCcw, Plus, X, Palette, ZoomIn, ZoomOut, ChevronDown, Globe } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import '@xterm/xterm/css/xterm.css';
import { createTauriListener } from '../../hooks/useTauriListener';
import { secureGetWithFallback, secureStoreAndClean } from '../../utils/secureStorage';

// ============ Terminal Themes ============

interface TerminalTheme {
    name: string;
    colors: Record<string, string>;
}

const TERMINAL_THEMES: Record<string, TerminalTheme> = {
    'tokyo-night': {
        name: 'Tokyo Night',
        colors: {
            background: '#1a1b26',
            foreground: '#c0caf5',
            cursor: '#7aa2f7',
            cursorAccent: '#1a1b26',
            selectionBackground: '#33467c',
            selectionForeground: '#c0caf5',
            black: '#15161e',
            red: '#f7768e',
            green: '#9ece6a',
            yellow: '#e0af68',
            blue: '#7aa2f7',
            magenta: '#bb9af7',
            cyan: '#7dcfff',
            white: '#a9b1d6',
            brightBlack: '#414868',
            brightRed: '#f7768e',
            brightGreen: '#9ece6a',
            brightYellow: '#e0af68',
            brightBlue: '#7aa2f7',
            brightMagenta: '#bb9af7',
            brightCyan: '#7dcfff',
            brightWhite: '#c0caf5',
        },
    },
    'dracula': {
        name: 'Dracula',
        colors: {
            background: '#282a36',
            foreground: '#f8f8f2',
            cursor: '#f8f8f2',
            cursorAccent: '#282a36',
            selectionBackground: '#44475a',
            selectionForeground: '#f8f8f2',
            black: '#21222c',
            red: '#ff5555',
            green: '#50fa7b',
            yellow: '#f1fa8c',
            blue: '#bd93f9',
            magenta: '#ff79c6',
            cyan: '#8be9fd',
            white: '#f8f8f2',
            brightBlack: '#6272a4',
            brightRed: '#ff6e6e',
            brightGreen: '#69ff94',
            brightYellow: '#ffffa5',
            brightBlue: '#d6acff',
            brightMagenta: '#ff92df',
            brightCyan: '#a4ffff',
            brightWhite: '#ffffff',
        },
    },
    'monokai': {
        name: 'Monokai',
        colors: {
            background: '#272822',
            foreground: '#f8f8f2',
            cursor: '#f8f8f0',
            cursorAccent: '#272822',
            selectionBackground: '#49483e',
            selectionForeground: '#f8f8f2',
            black: '#272822',
            red: '#f92672',
            green: '#a6e22e',
            yellow: '#f4bf75',
            blue: '#66d9ef',
            magenta: '#ae81ff',
            cyan: '#a1efe4',
            white: '#f8f8f2',
            brightBlack: '#75715e',
            brightRed: '#f92672',
            brightGreen: '#a6e22e',
            brightYellow: '#f4bf75',
            brightBlue: '#66d9ef',
            brightMagenta: '#ae81ff',
            brightCyan: '#a1efe4',
            brightWhite: '#f9f8f5',
        },
    },
    'solarized-dark': {
        name: 'Solarized Dark',
        colors: {
            background: '#002b36',
            foreground: '#839496',
            cursor: '#839496',
            cursorAccent: '#002b36',
            selectionBackground: '#073642',
            selectionForeground: '#93a1a1',
            black: '#073642',
            red: '#dc322f',
            green: '#859900',
            yellow: '#b58900',
            blue: '#268bd2',
            magenta: '#d33682',
            cyan: '#2aa198',
            white: '#eee8d5',
            brightBlack: '#586e75',
            brightRed: '#cb4b16',
            brightGreen: '#586e75',
            brightYellow: '#657b83',
            brightBlue: '#839496',
            brightMagenta: '#6c71c4',
            brightCyan: '#93a1a1',
            brightWhite: '#fdf6e3',
        },
    },
    'solarized-light': {
        name: 'Solarized Light',
        colors: {
            background: '#fdf6e3',
            foreground: '#657b83',
            cursor: '#657b83',
            cursorAccent: '#fdf6e3',
            selectionBackground: '#eee8d5',
            selectionForeground: '#586e75',
            black: '#073642',
            red: '#dc322f',
            green: '#859900',
            yellow: '#b58900',
            blue: '#268bd2',
            magenta: '#d33682',
            cyan: '#2aa198',
            white: '#eee8d5',
            brightBlack: '#586e75',
            brightRed: '#cb4b16',
            brightGreen: '#586e75',
            brightYellow: '#657b83',
            brightBlue: '#839496',
            brightMagenta: '#6c71c4',
            brightCyan: '#93a1a1',
            brightWhite: '#fdf6e3',
        },
    },
    'github-dark': {
        name: 'GitHub Dark',
        colors: {
            background: '#0d1117',
            foreground: '#c9d1d9',
            cursor: '#c9d1d9',
            cursorAccent: '#0d1117',
            selectionBackground: '#264f78',
            selectionForeground: '#ffffff',
            black: '#484f58',
            red: '#ff7b72',
            green: '#3fb950',
            yellow: '#d29922',
            blue: '#58a6ff',
            magenta: '#bc8cff',
            cyan: '#39c5cf',
            white: '#b1bac4',
            brightBlack: '#6e7681',
            brightRed: '#ffa198',
            brightGreen: '#56d364',
            brightYellow: '#e3b341',
            brightBlue: '#79c0ff',
            brightMagenta: '#d2a8ff',
            brightCyan: '#56d4dd',
            brightWhite: '#f0f6fc',
        },
    },
    'nord': {
        name: 'Nord',
        colors: {
            background: '#2e3440',
            foreground: '#d8dee9',
            cursor: '#d8dee9',
            cursorAccent: '#2e3440',
            selectionBackground: '#434c5e',
            selectionForeground: '#d8dee9',
            black: '#3b4252',
            red: '#bf616a',
            green: '#a3be8c',
            yellow: '#ebcb8b',
            blue: '#81a1c1',
            magenta: '#b48ead',
            cyan: '#88c0d0',
            white: '#e5e9f0',
            brightBlack: '#4c566a',
            brightRed: '#bf616a',
            brightGreen: '#a3be8c',
            brightYellow: '#ebcb8b',
            brightBlue: '#81a1c1',
            brightMagenta: '#b48ead',
            brightCyan: '#8fbcbb',
            brightWhite: '#eceff4',
        },
    },
    'catppuccin-mocha': {
        name: 'Catppuccin Mocha',
        colors: {
            background: '#1e1e2e',
            foreground: '#cdd6f4',
            cursor: '#f5e0dc',
            cursorAccent: '#1e1e2e',
            selectionBackground: '#45475a',
            selectionForeground: '#cdd6f4',
            black: '#45475a',
            red: '#f38ba8',
            green: '#a6e3a1',
            yellow: '#f9e2af',
            blue: '#89b4fa',
            magenta: '#f5c2e7',
            cyan: '#94e2d5',
            white: '#bac2de',
            brightBlack: '#585b70',
            brightRed: '#f38ba8',
            brightGreen: '#a6e3a1',
            brightYellow: '#f9e2af',
            brightBlue: '#89b4fa',
            brightMagenta: '#f5c2e7',
            brightCyan: '#94e2d5',
            brightWhite: '#a6adc8',
        },
    },
    'cyber': {
        name: 'Cyber',
        colors: {
            background: '#0a0e17',
            foreground: '#e0ffe0',
            cursor: '#00ff41',
            cursorAccent: '#0a0e17',
            selectionBackground: '#1a3a1a',
            selectionForeground: '#e0ffe0',
            black: '#0a0e17',
            red: '#ff0033',
            green: '#00ff41',
            yellow: '#ffb800',
            blue: '#00d4ff',
            magenta: '#ff00ff',
            cyan: '#00ffcc',
            white: '#e0ffe0',
            brightBlack: '#1a2a1a',
            brightRed: '#ff3366',
            brightGreen: '#39ff14',
            brightYellow: '#ffd000',
            brightBlue: '#00e5ff',
            brightMagenta: '#ff66ff',
            brightCyan: '#66ffcc',
            brightWhite: '#f0fff0',
        },
    },
};

const THEME_ORDER = ['tokyo-night', 'dracula', 'monokai', 'nord', 'catppuccin-mocha', 'github-dark', 'solarized-dark', 'solarized-light', 'cyber'];

// Map app themes to matching terminal themes
const APP_THEME_TO_TERMINAL: Record<string, string> = {
    'light': 'solarized-light',
    'dark': 'github-dark',
    'tokyo': 'tokyo-night',
    'cyber': 'cyber',
};

// ============ Tab State ============

interface TerminalTab {
    id: string;
    label: string;
    isConnected: boolean;
    isConnecting: boolean;
    type: 'local' | 'ssh';  // local PTY or SSH remote shell
}

let tabIdCounter = 0;
function nextTabId(): string {
    return `term-${++tabIdCounter}`;
}

// ============ Settings Persistence ============

const SETTINGS_KEY = 'aeroftp-terminal-settings';
// Vault account name for terminal settings. Anything not matching the
// server_/server_profile_/ai_apikey_/oauth_ prefixes lands in the
// `config_entries` category of the keystore export, so storing it here
// makes the terminal font/theme part of the AeroFTP backup automatically.
const TERMINAL_SETTINGS_VAULT_KEY = 'terminal_settings';
const SCROLLBACK_KEY = 'aeroftp-terminal-scrollback';
// L57: Size limits for scrollback persistence to prevent localStorage bloat
const SCROLLBACK_MAX_PER_TAB = 100 * 1024;  // 100 KB per tab
const SCROLLBACK_MAX_TOTAL = 500 * 1024;     // 500 KB total across all tabs

interface TerminalSettings {
    themeName: string;
    fontSize: number;
}

const DEFAULT_TERMINAL_SETTINGS: TerminalSettings = { themeName: 'tokyo-night', fontSize: 14 };

type PendingTerminalCommand = {
    command: string;
    insertOnly?: boolean;
};

// Synchronous fallback used as the React initial state — vault reads are
// async and we can't block the render. The full vault refresh happens
// right after mount in a useEffect (see `loadSettingsFromVault` below).
function loadSettingsSync(): TerminalSettings {
    try {
        const raw = localStorage.getItem(SETTINGS_KEY);
        if (raw) return { ...DEFAULT_TERMINAL_SETTINGS, ...JSON.parse(raw) };
    } catch { /* ignore */ }
    return DEFAULT_TERMINAL_SETTINGS;
}

async function loadSettingsFromVault(): Promise<TerminalSettings | null> {
    try {
        const v = await secureGetWithFallback<TerminalSettings>(
            TERMINAL_SETTINGS_VAULT_KEY,
            SETTINGS_KEY,
        );
        if (v) return { ...DEFAULT_TERMINAL_SETTINGS, ...v };
    } catch { /* ignore */ }
    return null;
}

function saveSettings(s: TerminalSettings) {
    // Keep localStorage as a synchronous mirror (fast first paint after reload)
    // and write to the vault so the keystore export picks it up.
    try { localStorage.setItem(SETTINGS_KEY, JSON.stringify(s)); } catch { /* ignore */ }
    void secureStoreAndClean(TERMINAL_SETTINGS_VAULT_KEY, SETTINGS_KEY, s).catch(() => { /* ignore */ });
}

// Scrollback persistence — save/restore terminal buffer text per tab
function saveScrollback(tabId: string, xterm: XTerm) {
    try {
        const buf = xterm.buffer.active;
        const lines: string[] = [];
        for (let i = 0; i < buf.length; i++) {
            const line = buf.getLine(i);
            if (line) lines.push(line.translateToString(true));
        }
        // Trim trailing empty lines
        while (lines.length > 0 && lines[lines.length - 1].trim() === '') lines.pop();
        if (lines.length === 0) return;
        let text = lines.join('\n');
        // L57: Truncate per-tab scrollback if it exceeds the size limit
        if (text.length > SCROLLBACK_MAX_PER_TAB) {
            text = text.slice(-SCROLLBACK_MAX_PER_TAB);
        }
        const allScrollbacks = JSON.parse(localStorage.getItem(SCROLLBACK_KEY) || '{}');
        allScrollbacks[tabId] = text;
        // Keep max 5 tabs worth of scrollback
        const keys = Object.keys(allScrollbacks);
        if (keys.length > 5) {
            delete allScrollbacks[keys[0]];
        }
        // L57: Enforce total size limit — evict oldest tabs until under budget
        let serialized = JSON.stringify(allScrollbacks);
        const sortedKeys = Object.keys(allScrollbacks);
        while (serialized.length > SCROLLBACK_MAX_TOTAL && sortedKeys.length > 1) {
            const oldest = sortedKeys.shift()!;
            if (oldest === tabId) continue; // Keep the current tab
            delete allScrollbacks[oldest];
            serialized = JSON.stringify(allScrollbacks);
        }
        localStorage.setItem(SCROLLBACK_KEY, serialized);
    } catch { /* quota exceeded or other error */ }
}

function loadScrollback(tabId: string): string | null {
    try {
        const allScrollbacks = JSON.parse(localStorage.getItem(SCROLLBACK_KEY) || '{}');
        return allScrollbacks[tabId] || null;
    } catch { return null; }
}

function clearScrollback(tabId: string) {
    try {
        const allScrollbacks = JSON.parse(localStorage.getItem(SCROLLBACK_KEY) || '{}');
        delete allScrollbacks[tabId];
        localStorage.setItem(SCROLLBACK_KEY, JSON.stringify(allScrollbacks));
    } catch { /* ignore */ }
}

// ============ Component ============

export interface SshConnectionInfo {
    host: string;
    port: number;
    username: string;
    password?: string;
    privateKeyPath?: string;
    keyPassphrase?: string;
}

interface SSHTerminalProps {
    className?: string;
    localPath?: string;
    sshConnection?: SshConnectionInfo | null;
    appTheme?: string;
    /** SEC-P1-06: TOFU host key check before SSH shell open */
    onCheckHostKey?: (host: string, port: number) => Promise<boolean>;
}

export const SSHTerminal: React.FC<SSHTerminalProps> = ({
    className = '',
    localPath = '~',
    sshConnection,
    appTheme,
    onCheckHostKey,
}) => {
    const t = useTranslation();
    // Settings
    const [settings, setSettings] = useState<TerminalSettings>(loadSettingsSync);
    // Hydrate from vault on mount (vault read is async; the localStorage
    // fallback above gives an instant first paint, then we upgrade if the
    // vault holds a fresher value).
    useEffect(() => {
        let cancelled = false;
        void loadSettingsFromVault().then(v => {
            if (!cancelled && v) setSettings(prev => ({ ...prev, ...v }));
        });
        return () => { cancelled = true; };
    }, []);
    // Track whether user manually picked a terminal theme (overrides auto-sync)
    const userOverrideRef = useRef(false);
    const [showThemeMenu, setShowThemeMenu] = useState(false);
    const themeMenuRef = useRef<HTMLDivElement>(null);

    // Tabs — start empty, user clicks "+" to create first tab
    const [tabs, setTabs] = useState<TerminalTab[]>([]);
    const [activeTabId, setActiveTabId] = useState<string>('');

    // Per-tab refs
    const xtermInstances = useRef<Map<string, XTerm>>(new Map());
    const fitAddons = useRef<Map<string, FitAddon>>(new Map());
    const unlistenFns = useRef<Map<string, () => void>>(new Map());
    const xtermDisposables = useRef<Map<string, IDisposable[]>>(new Map());
    const connectedTabs = useRef<Set<string>>(new Set());
    // Map tabId → pty session id (backend)
    const ptySessionIds = useRef<Map<string, string>>(new Map());
    // Pending command queued by AeroAgent when no terminal was open
    const pendingCommandRef = useRef<PendingTerminalCommand | null>(null);

    // One xterm container per tab. xterm.open(container) is called ONCE per
    // tab when its xterm instance is created, then we just show/hide the
    // container via CSS when switching tabs. The previous "single container +
    // detach/re-attach" approach broke xterm's renderer on tab switch (issue
    // reproduced by EhudKirsh: tab 1 went black after opening tab 2).
    const terminalContainers = useRef<Map<string, HTMLDivElement>>(new Map());
    // Kept for back-compat with code paths that still need *some* container
    // (e.g. ResizeObserver). It always points at the active tab's container.
    const terminalRef = useRef<HTMLDivElement>(null);
    const styleInjectedRef = useRef(false);

    const activeTab = tabs.find(t => t.id === activeTabId) || null;

    // Drops only the Tauri event listeners attached for this tab (pty-output,
    // ssh-shell-closed). Xterm-side handlers like `xterm.onData` are tied to
    // the xterm instance lifetime — they live until the tab is closed and are
    // released by `disposeXtermHandlers` below. Mixing the two pools caused
    // issue #125 on Linux: starting the shell called setupListener, which
    // called this function, which (when it also disposed xterm handlers)
    // killed the onData keystroke pipe right after the shell connected — the
    // terminal showed the prompt but silently dropped every key the user
    // typed. Symptom on Windows was hidden because spawn_shell stalled before
    // setupListener even ran.
    const disposeTabListeners = useCallback((tabId: string) => {
        const unlisten = unlistenFns.current.get(tabId);
        if (unlisten) {
            unlisten();
            unlistenFns.current.delete(tabId);
        }
    }, []);

    // Releases xterm-side handlers (onData, etc.) for a tab that's being torn
    // down. Call alongside disposeTabListeners on tab close / unmount.
    const disposeXtermHandlers = useCallback((tabId: string) => {
        const disposables = xtermDisposables.current.get(tabId);
        if (disposables) {
            for (const disposable of disposables) {
                disposable.dispose();
            }
            xtermDisposables.current.delete(tabId);
        }
    }, []);

    // Persist settings
    const updateSettings = useCallback((patch: Partial<TerminalSettings>) => {
        setSettings(prev => {
            const next = { ...prev, ...patch };
            saveSettings(next);
            return next;
        });
    }, []);

    // Close theme menu on outside click
    useEffect(() => {
        const handler = (e: MouseEvent) => {
            if (themeMenuRef.current && !themeMenuRef.current.contains(e.target as Node)) {
                setShowThemeMenu(false);
            }
        };
        if (showThemeMenu) document.addEventListener('mousedown', handler);
        return () => document.removeEventListener('mousedown', handler);
    }, [showThemeMenu]);

    // Get current theme
    const currentTheme = TERMINAL_THEMES[settings.themeName] || TERMINAL_THEMES['tokyo-night'];

    // Cleanup legacy cursor CSS if present
    useEffect(() => {
        const styleEl = document.getElementById('aeroftp-terminal-cursor-css');
        if (styleEl) styleEl.remove();
    }, []);

    // Auto-sync terminal theme with app theme (unless user manually picked one)
    useEffect(() => {
        if (!appTheme || userOverrideRef.current) return;
        const mapped = APP_THEME_TO_TERMINAL[appTheme];
        if (mapped && mapped !== settings.themeName && TERMINAL_THEMES[mapped]) {
            updateSettings({ themeName: mapped });
        }
    }, [appTheme]); // eslint-disable-line react-hooks/exhaustive-deps

    // Apply theme & font to all xterm instances
    useEffect(() => {
        xtermInstances.current.forEach((xterm) => {
            xterm.options.theme = currentTheme.colors;
            xterm.options.fontSize = settings.fontSize;
            xterm.refresh(0, xterm.rows - 1);
        });
        // Re-fit after font change
        fitAddons.current.forEach((fa) => {
            try { fa.fit(); } catch { /* container may not be visible */ }
        });
    }, [settings.themeName, settings.fontSize, currentTheme]);

    // Initialize / dispose xterm for each tab
    useEffect(() => {
        if (!activeTabId) return;
        const container = terminalContainers.current.get(activeTabId);
        if (!container) return;
        // Keep terminalRef in sync with the active tab's container so other
        // effects that still read terminalRef.current keep working.
        (terminalRef as React.MutableRefObject<HTMLDivElement | null>).current = container;

        const tabId = activeTabId;
        // Already initialized? Just focus + refit — DOM stays mounted.
        if (xtermInstances.current.has(tabId)) {
            const xterm = xtermInstances.current.get(tabId)!;
            xterm.focus();
            const fa = fitAddons.current.get(tabId);
            // Container was hidden until now, dimensions may have changed
            // while it was display:none. Refit on the next frame.
            setTimeout(() => {
                try { fa?.fit(); } catch { /* ignore */ }
                try { xterm.refresh(0, xterm.rows - 1); } catch { /* ignore */ }
            }, 0);
            return;
        }

        // Create new xterm
        const xterm = new XTerm({
            cols: 80,
            rows: 24,
            theme: currentTheme.colors,
            fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', 'Consolas', 'Courier New', 'Monaco', monospace",
            fontSize: settings.fontSize,
            fontWeight: '400',
            fontWeightBold: '600',
            letterSpacing: 0,
            lineHeight: 1.2,
            cursorBlink: true,
            cursorStyle: 'block',
            cursorInactiveStyle: 'block',
            scrollback: 10000,
            allowProposedApi: true,
            allowTransparency: false,
            convertEol: true,
            scrollOnUserInput: true,
            drawBoldTextInBrightColors: true,
        });

        const fitAddon = new FitAddon();
        const webLinksAddon = new WebLinksAddon();
        xterm.loadAddon(fitAddon);
        xterm.loadAddon(webLinksAddon);

        // Intercept Ctrl/Cmd + (-, =, +, 0) so xterm doesn't forward them to the
        // shell as control sequences. Returning false prevents PTY delivery; the
        // event still bubbles to the container's keydown listener which calls
        // updateSettings() to actually change the font size.
        xterm.attachCustomKeyEventHandler((e: KeyboardEvent) => {
            if (e.type !== 'keydown') return true;
            if (!(e.ctrlKey || e.metaKey) || e.altKey) return true;
            if (e.key === '-' || e.key === '_'
                || e.key === '=' || e.key === '+'
                || e.key === '0') {
                return false;
            }
            return true;
        });

        // Open xterm into THIS tab's dedicated container. Each tab keeps its
        // own DOM container mounted across tab switches; we just toggle
        // visibility via CSS, which avoids tearing down xterm's renderer.
        xterm.open(container);
        xterm.focus();

        setTimeout(() => {
            xterm.options.theme = currentTheme.colors;
            xterm.refresh(0, xterm.rows - 1);
        }, 50);
        xtermInstances.current.set(tabId, xterm);
        fitAddons.current.set(tabId, fitAddon);
        const terminalDisposables: IDisposable[] = [];
        xtermDisposables.current.set(tabId, terminalDisposables);

        // Wait for container to have actual dimensions before fitting + writing welcome
        // On first tab the DevTools panel is still laying out, so setTimeout alone is unreliable
        const writeWelcome = () => {
            fitAddon.fit();
            xterm.focus();
            const currentTab = tabs.find(t => t.id === tabId);
            if (currentTab?.type === 'ssh') {
                xterm.writeln('\x1b[1;36m╔════════════════════════════════════════╗\x1b[0m');
                xterm.writeln('\x1b[1;36m║\x1b[0m   \x1b[1;33mSSH Remote Shell\x1b[0m                     \x1b[1;36m║\x1b[0m');
                xterm.writeln('\x1b[1;36m╚════════════════════════════════════════╝\x1b[0m');
                xterm.writeln('');
                xterm.writeln(`\x1b[90m${t('devtools.terminalPanel.connectPrompt', { host: sshConnection?.host || t('devtools.terminalPanel.remoteServer') })}\x1b[0m`);
            } else {
                xterm.writeln('\x1b[1;35m╔════════════════════════════════════════╗\x1b[0m');
                xterm.writeln('\x1b[1;35m║\x1b[0m   \x1b[1;36mAeroFTP Terminal\x1b[0m                     \x1b[1;35m║\x1b[0m');
                xterm.writeln('\x1b[1;35m╚════════════════════════════════════════╝\x1b[0m');
                xterm.writeln('');
                xterm.writeln(`\x1b[90m${t('devtools.terminalPanel.launchPrompt')}\x1b[0m`);
            }
            xterm.writeln('');
        };

        if (container.offsetWidth > 0 && container.offsetHeight > 0) {
            // Container already has dimensions (subsequent tabs)
            setTimeout(writeWelcome, 50);
        } else {
            // First tab: wait for layout via ResizeObserver
            const ro = new ResizeObserver((entries) => {
                const entry = entries[0];
                if (entry && entry.contentRect.width > 0 && entry.contentRect.height > 0) {
                    ro.disconnect();
                    setTimeout(writeWelcome, 30);
                }
            });
            ro.observe(container);
        }

        // Handle keystrokes — route to PTY or SSH shell based on tab type
        terminalDisposables.push(
            xterm.onData(async (data) => {
                if (connectedTabs.current.has(tabId)) {
                    try {
                        const sessionId = ptySessionIds.current.get(tabId);
                        const tab = tabs.find(t => t.id === tabId);
                        if (tab?.type === 'ssh' && sessionId) {
                            await invoke('ssh_shell_write', { sessionId, data });
                        } else if (sessionId) {
                            await invoke('pty_write', { data, sessionId });
                        }
                    } catch (e) {
                        console.error('Terminal write error:', e);
                    }
                }
            })
        );

        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [activeTabId]);

    // Resize observer
    useEffect(() => {
        if (!terminalRef.current || !activeTabId) return;
        let resizeTimeout: number;
        const handleResize = () => {
            if (resizeTimeout) window.clearTimeout(resizeTimeout);
            resizeTimeout = window.setTimeout(() => {
                const fa = fitAddons.current.get(activeTabId);
                const xterm = xtermInstances.current.get(activeTabId);
                if (fa && xterm) {
                    fa.fit();
                    const dims = fa.proposeDimensions();
                    if (dims) {
                        const sessionId = ptySessionIds.current.get(activeTabId);
                        const tab = tabs.find(t => t.id === activeTabId);
                        if (tab?.type === 'ssh' && sessionId) {
                            invoke('ssh_shell_resize', { sessionId, cols: dims.cols, rows: dims.rows }).catch(() => {});
                        } else if (sessionId) {
                            invoke('pty_resize', { rows: dims.rows, cols: dims.cols, sessionId }).catch(() => {});
                        }
                    }
                }
            }, 100);
        };

        const observer = new ResizeObserver(() => handleResize());
        observer.observe(terminalRef.current);
        window.addEventListener('resize', handleResize);

        return () => {
            observer.disconnect();
            window.removeEventListener('resize', handleResize);
        };
    }, [activeTabId]);

    // Ctrl+/- font size (on the terminal container)
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if ((e.ctrlKey || e.metaKey) && (e.key === '=' || e.key === '+')) {
                e.preventDefault();
                updateSettings({ fontSize: Math.min(28, settings.fontSize + 1) });
            } else if ((e.ctrlKey || e.metaKey) && e.key === '-') {
                e.preventDefault();
                updateSettings({ fontSize: Math.max(8, settings.fontSize - 1) });
            } else if ((e.ctrlKey || e.metaKey) && e.key === '0') {
                e.preventDefault();
                updateSettings({ fontSize: 14 });
            }
        };
        const container = terminalRef.current;
        container?.addEventListener('keydown', handler);
        return () => container?.removeEventListener('keydown', handler);
    }, [settings.fontSize, updateSettings]);

    // Setup PTY listener for a tab
    const setupListener = useCallback((tabId: string) => {
        // Clean previous
        disposeTabListeners(tabId);

        const sessionId = ptySessionIds.current.get(tabId);
        const eventName = sessionId ? `pty-output-${sessionId}` : 'pty-output';

        const outputUnlisten = createTauriListener<string>(eventName, (event) => {
            const xterm = xtermInstances.current.get(tabId);
            if (xterm) xterm.write(event.payload);
        });

        // Listen for SSH shell close events
        if (sessionId?.startsWith('ssh-shell-')) {
            const closeUnlisten = createTauriListener<string>(`ssh-shell-closed-${sessionId}`, () => {
                connectedTabs.current.delete(tabId);
                ptySessionIds.current.delete(tabId);
                setTabs(prev => prev.map(t => t.id === tabId ? { ...t, isConnected: false, isConnecting: false } : t));
                const xterm = xtermInstances.current.get(tabId);
                if (xterm) {
                    xterm.writeln('');
                    xterm.writeln('\x1b[33mSSH connection closed.\x1b[0m');
                }
            });
            unlistenFns.current.set(tabId, () => {
                outputUnlisten();
                closeUnlisten();
            });
        } else {
            unlistenFns.current.set(tabId, outputUnlisten);
        }
    }, [disposeTabListeners]);

    // Start shell for active tab (local PTY or SSH remote)
    const startShell = useCallback(async () => {
        const tabId = activeTabId;
        const tab = tabs.find(t => t.id === tabId);
        if (!tab || tab.isConnecting || tab.isConnected) return;

        setTabs(prev => prev.map(t => t.id === tabId ? { ...t, isConnecting: true } : t));

        try {
            let sessionId: string | undefined;

            if (tab.type === 'ssh' && sshConnection) {
                // SEC-P1-06: TOFU host key check before SSH shell open
                if (onCheckHostKey) {
                    const accepted = await onCheckHostKey(sshConnection.host, sshConnection.port);
                    if (!accepted) {
                        setTabs(prev => prev.map(t => t.id === tabId ? { ...t, isConnecting: false } : t));
                        return;
                    }
                }
                // Open SSH remote shell
                const result = await invoke<string>('ssh_shell_open', {
                    host: sshConnection.host,
                    port: sshConnection.port,
                    username: sshConnection.username,
                    password: sshConnection.password || null,
                    privateKeyPath: sshConnection.privateKeyPath || null,
                    keyPassphrase: sshConnection.keyPassphrase || null,
                });
                const match = result.match(/\[session:([^\]]+)\]/);
                sessionId = match ? match[1] : undefined;
            } else {
                // Spawn local PTY shell
                const cwdToUse = (localPath && localPath !== '~') ? localPath : null;
                const result = await invoke<string>('spawn_shell', { cwd: cwdToUse });
                const match = result.match(/\[session:([^\]]+)\]/);
                sessionId = match ? match[1] : undefined;
            }

            if (sessionId) {
                ptySessionIds.current.set(tabId, sessionId);
            }

            setupListener(tabId);

            connectedTabs.current.add(tabId);
            setTabs(prev => prev.map(t => t.id === tabId ? { ...t, isConnected: true, isConnecting: false } : t));

            const xterm = xtermInstances.current.get(tabId);
            if (xterm) {
                xterm.clear();
                xterm.writeln('');
            }

            // Resize (local PTY only — SSH shell resize is a no-op currently)
            if (tab.type !== 'ssh') {
                const fa = fitAddons.current.get(tabId);
                if (fa) {
                    const dims = fa.proposeDimensions();
                    if (dims) {
                        if (sessionId) {
                            await invoke('pty_resize', { rows: dims.rows, cols: dims.cols, sessionId });
                        }
                    }
                }
            }

            xtermInstances.current.get(tabId)?.focus();

            // Clear screen for local shell (prompt is set at spawn time in pty.rs)
            if (tab.type !== 'ssh') {
                setTimeout(async () => {
                    try {
                        const isWindows = navigator.platform.startsWith('Win');
                        // Windows: cls for cmd.exe/PowerShell; Linux: clear for bash/zsh
                        const clearCommand = isWindows ? 'cls\r\n' : 'clear\n';
                        if (sessionId) {
                            await invoke('pty_write', { data: clearCommand, sessionId });
                        }
                    } catch { /* ignore */ }
                }, 300);
            }

            // Execute pending command queued by AeroAgent (auto-start flow)
            if (pendingCommandRef.current) {
                const pendingCmd = pendingCommandRef.current;
                pendingCommandRef.current = null;
                // Wait for shell prompt to be ready (after clear screen)
                setTimeout(async () => {
                    try {
                        const data = pendingCmd.insertOnly ? pendingCmd.command : pendingCmd.command + '\n';
                        if (tab.type === 'ssh' && sessionId) {
                            await invoke('ssh_shell_write', { sessionId, data });
                        } else {
                            if (sessionId) {
                                await invoke('pty_write', { data, sessionId });
                            }
                        }
                    } catch { /* ignore */ }
                }, 600);
            }

        } catch (e) {
            const xterm = xtermInstances.current.get(tabId);
            if (xterm) xterm.writeln(`\x1b[31mError: ${e}\x1b[0m`);
            setTabs(prev => prev.map(t => t.id === tabId ? { ...t, isConnecting: false } : t));
        }
    }, [activeTabId, tabs, localPath, sshConnection, setupListener]);

    // Stop shell for active tab
    const stopShell = useCallback(async () => {
        const tabId = activeTabId;
        const tab = tabs.find(t => t.id === tabId);
        disposeTabListeners(tabId);

        try {
            const sessionId = ptySessionIds.current.get(tabId);
            if (tab?.type === 'ssh' && sessionId) {
                await invoke('ssh_shell_close', { sessionId });
            } else if (sessionId) {
                await invoke('pty_close', { sessionId });
            }
        } catch { /* ignore */ }

        connectedTabs.current.delete(tabId);
        ptySessionIds.current.delete(tabId);
        setTabs(prev => prev.map(t => t.id === tabId ? { ...t, isConnected: false, isConnecting: false } : t));

        const xterm = xtermInstances.current.get(tabId);
        if (xterm) {
            xterm.writeln('');
            xterm.writeln(`\x1b[33m${t('devtools.terminalPanel.closed')}\x1b[0m`);
            xterm.writeln(`\x1b[90m${t('devtools.terminalPanel.newShellPrompt')}\x1b[0m`);
        }
    }, [activeTabId, tabs, disposeTabListeners]);

    const restartShell = useCallback(async () => {
        await stopShell();
        setTimeout(startShell, 500);
    }, [stopShell, startShell]);

    // Add local tab
    const addTab = useCallback(() => {
        const id = nextTabId();
        const num = tabs.length + 1;
        setTabs(prev => [...prev, { id, label: `Terminal ${num}`, isConnected: false, isConnecting: false, type: 'local' }]);
        setActiveTabId(id);
    }, [tabs.length]);

    // Add SSH shell tab
    const addSshTab = useCallback(() => {
        if (!sshConnection) return;
        const id = nextTabId();
        const label = `SSH ${sshConnection.host}`;
        setTabs(prev => [...prev, { id, label, isConnected: false, isConnecting: false, type: 'ssh' }]);
        setActiveTabId(id);
    }, [sshConnection]);

    // Close tab
    const closeTab = useCallback((tabId: string) => {
        // Clean up resources
        disposeTabListeners(tabId);
        disposeXtermHandlers(tabId);
        const sessionId = ptySessionIds.current.get(tabId);
        const tab = tabs.find(t => t.id === tabId);
        if (connectedTabs.current.has(tabId)) {
            if (tab?.type === 'ssh' && sessionId) {
                invoke('ssh_shell_close', { sessionId }).catch(() => {});
            } else if (sessionId) {
                invoke('pty_close', { sessionId }).catch(() => {});
            }
        }
        connectedTabs.current.delete(tabId);
        ptySessionIds.current.delete(tabId);
        const xterm = xtermInstances.current.get(tabId);
        if (xterm) {
            saveScrollback(tabId, xterm);
            xterm.dispose();
        }
        xtermInstances.current.delete(tabId);
        fitAddons.current.delete(tabId);

        setTabs(prev => {
            const remaining = prev.filter(t => t.id !== tabId);
            if (remaining.length === 0) {
                setActiveTabId('');
            } else if (activeTabId === tabId) {
                setActiveTabId(remaining[remaining.length - 1].id);
            }
            return remaining;
        });
    }, [activeTabId, disposeTabListeners, disposeXtermHandlers, tabs]);

    // Cleanup on unmount — save scrollback for all tabs
    useEffect(() => {
        return () => {
            xtermInstances.current.forEach((xterm, tabId) => {
                saveScrollback(tabId, xterm);
                disposeTabListeners(tabId);
                disposeXtermHandlers(tabId);
                xterm.dispose();
            });
            connectedTabs.current.forEach((tabId) => {
                const sessionId = ptySessionIds.current.get(tabId);
                if (sessionId) {
                    invoke('pty_close', { sessionId }).catch(() => {});
                }
            });
        };
    }, [disposeTabListeners, disposeXtermHandlers]);

    // Listen for terminal-execute events from AeroAgent
    useEffect(() => {
        const handleTerminalExecute = (e: Event) => {
            const { command, displayOnly, insertOnly } = (e as CustomEvent).detail;
            if (!command) return;

            // displayOnly: shell_execute already ran the command in Rust backend.
            // Only display a visual note in the terminal — do NOT write to PTY (H34 audit fix).
            if (displayOnly) {
                const tabId = activeTabId;
                const targetTab = tabId && connectedTabs.current.has(tabId)
                    ? tabId
                    : Array.from(connectedTabs.current)[0] || null;
                if (targetTab) {
                    const xterm = xtermInstances.current.get(targetTab);
                    if (xterm) {
                        // Display as dim comment — ANSI dim (2m) + reset (0m)
                        xterm.write(`\r\n\x1b[2m# [AeroAgent] executed: ${command}\x1b[0m\r\n`);
                    }
                }
                return;
            }

            // Find active connected tab
            const tabId = activeTabId;
            if (!tabId || !connectedTabs.current.has(tabId)) {
                // No active terminal — try to find any connected tab
                const connectedTabIds = Array.from(connectedTabs.current);
                if (connectedTabIds.length === 0) {
                    // No connected terminals — auto-create a local tab and queue the command
                    pendingCommandRef.current = { command, insertOnly };
                    addTab();
                    return;
                }
                // Use the first connected tab
                const targetTab = connectedTabIds[0];
                const sessionId = ptySessionIds.current.get(targetTab);
                const tab = tabs.find(t => t.id === targetTab);
                const data = insertOnly ? command : command + '\n';
                if (tab?.type === 'ssh' && sessionId) {
                    invoke('ssh_shell_write', { sessionId, data }).catch(() => {});
                } else if (sessionId) {
                    invoke('pty_write', { data, sessionId }).catch(() => {});
                }
                setActiveTabId(targetTab);
                return;
            }

            // Write to active tab
            const sessionId = ptySessionIds.current.get(tabId);
            const tab = tabs.find(t => t.id === tabId);
            const data = insertOnly ? command : command + '\n';
            if (tab?.type === 'ssh' && sessionId) {
                invoke('ssh_shell_write', { sessionId, data }).catch(() => {});
            } else if (sessionId) {
                invoke('pty_write', { data, sessionId }).catch(() => {});
            }
        };

        window.addEventListener('terminal-execute', handleTerminalExecute);
        return () => window.removeEventListener('terminal-execute', handleTerminalExecute);
    }, [activeTabId, tabs, addTab]);

    // Auto-start shell when AeroAgent queued a command on a new tab
    useEffect(() => {
        if (!pendingCommandRef.current) return;
        const tab = tabs.find(t => t.id === activeTabId);
        if (tab && !tab.isConnected && !tab.isConnecting) {
            startShell();
        }
    }, [activeTabId, tabs, startShell]);

    return (
        <div className={`flex flex-col h-full ${className}`} style={{ backgroundColor: currentTheme.colors.background }}>
            {/* Toolbar: tabs + controls */}
            <div className="flex items-center justify-between px-2 py-1 bg-[#161b22] border-b border-[#30363d] flex-shrink-0">
                {/* Tabs */}
                <div className="flex items-center gap-0.5 overflow-x-auto flex-1 min-w-0">
                    {tabs.map(tab => (
                        <div
                            key={tab.id}
                            className={`flex items-center gap-1 px-2 py-1 text-xs rounded-t cursor-pointer border-b-2 transition-colors ${
                                tab.id === activeTabId
                                    ? 'bg-[#1a1b26] text-gray-200 border-green-400'
                                    : 'bg-transparent text-gray-500 border-transparent hover:text-gray-300 hover:bg-[#1a1b26]/50'
                            }`}
                            onClick={() => setActiveTabId(tab.id)}
                        >
                            {tab.type === 'ssh'
                                ? <Globe size={10} className={tab.isConnected ? 'text-cyan-400' : 'text-gray-500'} />
                                : <TerminalIcon size={10} className={tab.isConnected ? 'text-green-400' : 'text-gray-500'} />
                            }
                            <span className="font-mono whitespace-nowrap">{tab.label}</span>
                            <button
                                onClick={(e) => { e.stopPropagation(); closeTab(tab.id); }}
                                className="ml-0.5 text-gray-500 hover:text-red-400 transition-colors"
                            >
                                <X size={10} />
                            </button>
                        </div>
                    ))}
                    <button
                        onClick={addTab}
                        className="flex items-center px-1.5 py-1 text-gray-500 hover:text-gray-300 transition-colors"
                        title="New terminal tab"
                    >
                        <Plus size={12} />
                    </button>
                    {sshConnection && (
                        <button
                            onClick={addSshTab}
                            className="flex items-center gap-0.5 px-1.5 py-1 text-gray-500 hover:text-cyan-300 transition-colors"
                            title={`SSH shell to ${sshConnection.host}`}
                        >
                            <Globe size={12} />
                            <span className="text-[10px]">SSH</span>
                        </button>
                    )}
                </div>

                {/* Controls */}
                <div className="flex items-center gap-1 flex-shrink-0 ml-2">
                    {/* Theme selector */}
                    <div className="relative" ref={themeMenuRef}>
                        <button
                            onClick={() => setShowThemeMenu(!showThemeMenu)}
                            className="flex items-center gap-1 px-1.5 py-1 text-xs text-gray-400 hover:text-gray-200 transition-colors"
                            title="Terminal theme"
                        >
                            <Palette size={12} />
                            <ChevronDown size={10} />
                        </button>
                        {showThemeMenu && (
                            <div className="absolute right-0 top-full mt-1 bg-[#1e1e2e] border border-gray-600 rounded shadow-xl z-50 min-w-[160px]">
                                {THEME_ORDER.map(key => {
                                    const t = TERMINAL_THEMES[key];
                                    return (
                                        <button
                                            key={key}
                                            onClick={() => { userOverrideRef.current = true; updateSettings({ themeName: key }); setShowThemeMenu(false); }}
                                            className={`w-full text-left px-3 py-1.5 text-xs hover:bg-gray-700 transition-colors flex items-center gap-2 ${
                                                settings.themeName === key ? 'text-green-400' : 'text-gray-300'
                                            }`}
                                        >
                                            <span
                                                className="w-3 h-3 rounded-sm border border-gray-600 flex-shrink-0"
                                                style={{ backgroundColor: t.colors.background }}
                                            />
                                            {t.name}
                                            {settings.themeName === key && <span className="ml-auto text-green-400">●</span>}
                                        </button>
                                    );
                                })}
                            </div>
                        )}
                    </div>

                    {/* Font size */}
                    <button
                        onClick={() => updateSettings({ fontSize: Math.max(8, settings.fontSize - 1) })}
                        className="p-1 text-gray-400 hover:text-gray-200 transition-colors"
                        title="Decrease font size (Ctrl+-)"
                    >
                        <ZoomOut size={12} />
                    </button>
                    <span className="text-[10px] text-gray-500 font-mono w-5 text-center">{settings.fontSize}</span>
                    <button
                        onClick={() => updateSettings({ fontSize: Math.min(28, settings.fontSize + 1) })}
                        className="p-1 text-gray-400 hover:text-gray-200 transition-colors"
                        title="Increase font size (Ctrl+=)"
                    >
                        <ZoomIn size={12} />
                    </button>

                    <div className="w-px h-4 bg-gray-700 mx-1" />

                    {/* Shell controls */}
                    {activeTab && !activeTab.isConnected ? (
                        <button
                            onClick={startShell}
                            disabled={activeTab.isConnecting}
                            className="flex items-center gap-1 px-2 py-1 text-xs bg-green-600 hover:bg-green-500 disabled:bg-gray-600 text-white rounded transition-colors"
                            title={t('devtools.terminalPanel.start')}
                        >
                            <Play size={12} />
                            {activeTab.isConnecting ? t('devtools.terminalPanel.starting') : t('devtools.terminalPanel.start')}
                        </button>
                    ) : activeTab?.isConnected ? (
                        <>
                            <button
                                onClick={restartShell}
                                className="flex items-center gap-1 px-1.5 py-1 text-xs bg-yellow-600 hover:bg-yellow-500 text-white rounded transition-colors"
                                title={t('devtools.terminalPanel.restart')}
                            >
                                <RotateCcw size={12} />
                            </button>
                            <button
                                onClick={stopShell}
                                className="flex items-center gap-1 px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded transition-colors"
                                title={t('devtools.terminalPanel.stopShell')}
                            >
                                <Square size={12} />
                                {t('devtools.terminalPanel.stop')}
                            </button>
                        </>
                    ) : null}
                </div>
            </div>

            {/* Terminal area */}
            {tabs.length === 0 ? (
                <div
                    className="flex-1 flex flex-col items-center justify-center gap-3"
                    style={{ backgroundColor: currentTheme.colors.background }}
                >
                    <TerminalIcon size={32} className="text-gray-600" />
                    <span className="text-gray-500 text-sm">Click <strong>+</strong> to open a terminal</span>
                    {sshConnection && (
                        <span className="text-gray-600 text-xs">or <strong>SSH</strong> for remote shell to {sshConnection.host}</span>
                    )}
                </div>
            ) : (
                <div
                    className="flex-1 relative overflow-hidden"
                    style={{ backgroundColor: currentTheme.colors.background }}
                    onClick={() => xtermInstances.current.get(activeTabId)?.focus()}
                >
                    {tabs.map(tab => (
                        <div
                            key={tab.id}
                            ref={(el) => {
                                if (el) terminalContainers.current.set(tab.id, el);
                                else terminalContainers.current.delete(tab.id);
                            }}
                            className={`absolute inset-0 p-1 cursor-text ${
                                tab.id === activeTabId ? '' : 'invisible pointer-events-none'
                            }`}
                        />
                    ))}
                </div>
            )}
        </div>
    );
};

export default SSHTerminal;
