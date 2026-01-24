import React, { useEffect, useRef, useState } from 'react';
import { Terminal as XTerm } from 'xterm';
import { FitAddon } from '@xterm/addon-fit';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { Terminal as TerminalIcon, Play, Square, RotateCcw } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import 'xterm/css/xterm.css';

// Custom terminal styles (Tokyo Night theme)
const terminalStyles = `
.xterm .xterm-cursor-layer {
    z-index: 10;
}
.xterm .xterm-cursor {
    background-color: #7aa2f7 !important;
    opacity: 1 !important;
}
.xterm .xterm-cursor-block {
    background-color: #7aa2f7 !important;
}
.xterm .xterm-cursor-outline {
    outline: 2px solid #7aa2f7 !important;
}
`;

interface SSHTerminalProps {
    className?: string;
    localPath?: string;
}

export const SSHTerminal: React.FC<SSHTerminalProps> = ({
    className = '',
    localPath = '~',
}) => {
    const terminalRef = useRef<HTMLDivElement>(null);
    const xtermRef = useRef<XTerm | null>(null);
    const fitAddonRef = useRef<FitAddon | null>(null);
    const [isConnected, setIsConnected] = useState(false);
    const [isConnecting, setIsConnecting] = useState(false);
    const unlistenRef = useRef<UnlistenFn | null>(null);
    const isConnectedRef = useRef(false); // Ref to track connection state in callbacks
    const styleInjectedRef = useRef(false);

    // Inject custom styles once
    useEffect(() => {
        if (!styleInjectedRef.current) {
            const styleElement = document.createElement('style');
            styleElement.textContent = terminalStyles;
            document.head.appendChild(styleElement);
            styleInjectedRef.current = true;
        }
    }, []);

    // Initialize xterm.js
    useEffect(() => {
        if (!terminalRef.current) return;

        // Cleanup previous instance if exists
        if (xtermRef.current) {
            xtermRef.current.dispose();
            xtermRef.current = null;
        }

        const xterm = new XTerm({
            cols: 80,
            rows: 24,
            theme: {
                background: '#1a1b26',      // Tokyo Night background
                foreground: '#c0caf5',      // Light lavender text
                cursor: '#7aa2f7',          // Blue cursor (visible)
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
            fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', 'Consolas', 'Monaco', monospace",
            fontSize: 14,
            fontWeight: '400',
            fontWeightBold: '600',
            letterSpacing: 0,
            lineHeight: 1.2,
            cursorBlink: true,
            cursorStyle: 'block',           // Block cursor for visibility
            cursorInactiveStyle: 'block',   // Keep block even when not focused
            scrollback: 5000,
            allowProposedApi: true,
            convertEol: true,
            scrollOnUserInput: true,
        });

        const fitAddon = new FitAddon();
        const webLinksAddon = new WebLinksAddon();

        xterm.loadAddon(fitAddon);
        xterm.loadAddon(webLinksAddon);
        xterm.open(terminalRef.current);

        // Focus immediately to show cursor
        xterm.focus();
        
        // Force re-apply theme after open (fixes WebKit rendering issues)
        setTimeout(() => {
            xterm.options.theme = {
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
            };
            xterm.refresh(0, xterm.rows - 1);
        }, 50);

        // IMPORTANT: Fit must be called after a slight delay to ensure container is rendered
        setTimeout(() => {
            fitAddon.fit();
            console.log('PTY: Initial fit complete');
        }, 100);

        xtermRef.current = xterm;
        fitAddonRef.current = fitAddon;

        // Welcome message
        xterm.writeln('\x1b[1;35m╔════════════════════════════════════════╗\x1b[0m');
        xterm.writeln('\x1b[1;35m║\x1b[0m   \x1b[1;36mAeroFTP Terminal\x1b[0m                     \x1b[1;35m║\x1b[0m');
        xterm.writeln('\x1b[1;35m╚════════════════════════════════════════╝\x1b[0m');
        xterm.writeln('');
        xterm.writeln('\x1b[90mClick "Start" to launch your shell.\x1b[0m');
        xterm.writeln('');

        // Handle keystrokes - send to PTY
        xterm.onData(async (data) => {
            console.log('PTY: onData fired, connected:', isConnectedRef.current, 'data:', JSON.stringify(data));
            if (isConnectedRef.current) {
                try {
                    await invoke('pty_write', { data });
                    console.log('PTY: write success');
                } catch (e) {
                    console.error('PTY write error:', e);
                }
            } else {
                console.warn('PTY: Not connected, ignoring keystroke');
            }
        });

        // Handle resize with ResizeObserver (detects container size changes, not just window)
        let resizeTimeout: number;
        const handleResize = () => {
            if (resizeTimeout) window.clearTimeout(resizeTimeout);
            resizeTimeout = window.setTimeout(() => {
                if (fitAddonRef.current && xtermRef.current) {
                    fitAddonRef.current.fit();
                    const dims = fitAddonRef.current.proposeDimensions();
                    if (dims) {
                        console.log('PTY: Resizing to', dims);
                        invoke('pty_resize', { rows: dims.rows, cols: dims.cols }).catch(console.error);
                    }
                }
            }, 100);
        };

        // Use ResizeObserver to detect when the terminal container resizes
        const resizeObserver = new ResizeObserver(() => {
            handleResize();
        });

        if (terminalRef.current) {
            resizeObserver.observe(terminalRef.current);
        }

        // Also listen to window resize as fallback
        window.addEventListener('resize', handleResize);

        return () => {
            window.removeEventListener('resize', handleResize);
            resizeObserver.disconnect();
            if (unlistenRef.current) {
                unlistenRef.current();
            }
            xterm.dispose();
            xtermRef.current = null;
        };
    }, []); // Only run once on mount (removed isConnected dep to avoid re-init)

    // Setup event listener for PTY output
    const setupListener = async () => {
        if (unlistenRef.current) {
            unlistenRef.current();
        }

        try {
            unlistenRef.current = await listen<string>('pty-output', (event) => {
                if (xtermRef.current) {
                    xtermRef.current.write(event.payload);
                }
            });
        } catch (e) {
            console.error('PTY: Failed to setup listener:', e);
        }
    };

    // Start shell
    const startShell = async () => {
        if (isConnecting || isConnected) return;

        setIsConnecting(true);
        console.log('PTY: Starting shell in:', localPath);

        try {
            await setupListener();

            // Pass localPath as cwd to the backend
            // If localPath is '~' or empty, backend will use default
            const cwdToUse = (localPath && localPath !== '~') ? localPath : null;

            const result = await invoke<string>('spawn_shell', { cwd: cwdToUse });
            console.log('PTY: Shell spawned:', result);
            setIsConnected(true);
            isConnectedRef.current = true; // Update ref for callbacks

            if (xtermRef.current) {
                xtermRef.current.clear();
                xtermRef.current.writeln('');
            }

            // Notify PTY of initial size
            if (fitAddonRef.current) {
                const dims = fitAddonRef.current.proposeDimensions();
                if (dims) {
                    await invoke('pty_resize', { rows: dims.rows, cols: dims.cols });
                }
            }

            // Focus terminal
            xtermRef.current?.focus();

            // Send custom PS1 after a short delay to override .bashrc
            setTimeout(async () => {
                try {
                    // Set colorful prompt: green user@host, blue path
                    const ps1Command = `export PS1='\\[\\e[1;32m\\]\\u@\\h\\[\\e[0m\\]:\\[\\e[1;34m\\]\\w\\[\\e[0m\\]\\$ ' && clear\n`;
                    await invoke('pty_write', { data: ps1Command });
                } catch (e) {
                    console.error('Failed to set PS1:', e);
                }
            }, 300);

        } catch (e) {
            if (xtermRef.current) {
                xtermRef.current.writeln(`\x1b[31m✗ Error: ${e}\x1b[0m`);
            }
        } finally {
            setIsConnecting(false);
        }
    };

    // Stop shell
    const stopShell = async () => {
        if (unlistenRef.current) {
            unlistenRef.current();
            unlistenRef.current = null;
        }

        try {
            await invoke('pty_close');
        } catch (e) {
            console.error('PTY close error:', e);
        }

        setIsConnected(false);
        isConnectedRef.current = false; // Update ref for callbacks

        if (xtermRef.current) {
            xtermRef.current.writeln('');
            xtermRef.current.writeln('\x1b[33mTerminal closed.\x1b[0m');
            xtermRef.current.writeln('\x1b[90mClick "Start" to launch a new shell.\x1b[0m');
        }
    };

    // Restart shell
    const restartShell = async () => {
        await stopShell();
        setTimeout(startShell, 500); // Give a bit more time for cleanup
    };

    // Re-fit on visibility
    useEffect(() => {
        if (fitAddonRef.current) {
            setTimeout(() => fitAddonRef.current?.fit(), 50);
        }
    }, []);

    return (
        <div className={`flex flex-col h-full bg-[#0d1117] ${className}`}>
            <div className="flex items-center justify-between px-4 py-2 bg-[#161b22] border-b border-[#30363d]">
                <div className="flex items-center gap-2 text-sm text-gray-300">
                    <TerminalIcon size={14} className={isConnected ? 'text-green-400' : 'text-gray-400'} />
                    <span className="font-medium font-mono">Terminal</span>
                    <span className={`text-xs font-mono ${isConnected ? 'text-green-400' : 'text-gray-500'}`}>
                        {isConnected ? '● Connected' : '○ Disconnected'}
                    </span>
                </div>

                <div className="flex items-center gap-2">
                    {!isConnected ? (
                        <button
                            onClick={startShell}
                            disabled={isConnecting}
                            className="flex items-center gap-1 px-2 py-1 text-xs bg-green-600 hover:bg-green-500 disabled:bg-gray-600 text-white rounded transition-colors"
                            title="Start shell"
                        >
                            <Play size={12} />
                            {isConnecting ? 'Starting...' : 'Start'}
                        </button>
                    ) : (
                        <>
                            <button
                                onClick={restartShell}
                                className="flex items-center gap-1 px-2 py-1 text-xs bg-yellow-600 hover:bg-yellow-500 text-white rounded transition-colors"
                                title="Restart shell"
                            >
                                <RotateCcw size={12} />
                            </button>
                            <button
                                onClick={stopShell}
                                className="flex items-center gap-1 px-2 py-1 text-xs bg-red-600 hover:bg-red-500 text-white rounded transition-colors"
                                title="Stop shell"
                            >
                                <Square size={12} />
                                Stop
                            </button>
                        </>
                    )}
                </div>
            </div>
            <div 
                ref={terminalRef} 
                className="flex-1 p-1 overflow-hidden cursor-text bg-[#0d1117]" 
                onClick={() => xtermRef.current?.focus()}
            />
        </div>
    );
};

export default SSHTerminal;
