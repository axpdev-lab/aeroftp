import * as React from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Minus, Square, X, Maximize2, Heart, Settings, Lock, LockOpen, LogOut, Cloud } from 'lucide-react';
import { useTranslation } from '../i18n';
import { ThemeToggle } from '../hooks/useTheme';
import { VaultIcon } from './icons/VaultIcon';
import type { Theme, EffectiveTheme } from '../hooks/useTheme';

// ─── Types ──────────────────────────────────────────────────────────────────

interface MenuItem {
    label: string;
    shortcut?: string;
    onClick: () => void;
    separator?: false;
    disabled?: boolean;
}

interface MenuSeparator {
    separator: true;
}

type MenuEntry = MenuItem | MenuSeparator;

interface TitlebarProps {
    appTheme: EffectiveTheme;
    theme: Theme;
    setTheme: (t: Theme) => void;
    isConnected: boolean;
    onDisconnect: () => void;
    onShowConnectionScreen: () => void;
    showConnectionScreen: boolean;
    onOpenSettings: () => void;
    onShowSupport: () => void;
    onShowCyberTools: () => void;
    onShowVault: () => void;
    onShowAbout: () => void;
    onShowShortcuts: () => void;
    onShowDependencies: () => void;
    masterPasswordSet: boolean;
    onLockApp: () => void;
    onSetupMasterPassword: () => void;
    onRefresh: () => void;
    onNewFolder: () => void;
    onToggleDevTools: () => void;
    onToggleTheme: () => void;
    onToggleDebugMode: () => void;
    onRename: () => void;
    onDelete: () => void;
    onSelectAll: () => void;
    onCut: () => void;
    onCopy: () => void;
    onPaste: () => void;
    hasSelection: boolean;
    hasClipboard: boolean;
    onToggleEditor: () => void;
    onToggleTerminal: () => void;
    onToggleAgent: () => void;
    onQuit: () => void;
    hasActivity: boolean;
}

// ─── TitlebarMenu sub-component ─────────────────────────────────────────────

interface TitlebarMenuProps {
    label: string;
    items: MenuEntry[];
    isOpen: boolean;
    onOpen: () => void;
    onClose: () => void;
    anyMenuOpen: boolean;
}

const TitlebarMenu: React.FC<TitlebarMenuProps> = ({ label, items, isOpen, onOpen, onClose, anyMenuOpen }) => {
    const menuRef = React.useRef<HTMLDivElement>(null);

    React.useEffect(() => {
        if (!isOpen) return;
        const handleClick = (e: MouseEvent) => {
            if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
                onClose();
            }
        };
        const handleKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('mousedown', handleClick);
        document.addEventListener('keydown', handleKey);
        return () => {
            document.removeEventListener('mousedown', handleClick);
            document.removeEventListener('keydown', handleKey);
        };
    }, [isOpen, onClose]);

    return (
        <div ref={menuRef} className="relative">
            <button
                onClick={() => isOpen ? onClose() : onOpen()}
                onMouseEnter={() => { if (anyMenuOpen && !isOpen) onOpen(); }}
                className={`px-2.5 h-9 text-xs transition-colors cursor-pointer select-none ${
                    isOpen
                        ? 'bg-[var(--color-bg-tertiary)] text-[var(--color-text-primary)]'
                        : 'text-[var(--color-text-secondary)] hover:bg-[var(--color-bg-tertiary)] hover:text-[var(--color-text-primary)]'
                }`}
            >
                {label}
            </button>
            {isOpen && (
                <div className="absolute top-full left-0 min-w-[240px] py-1.5 bg-[var(--color-bg-secondary)] border border-[var(--color-border)] rounded-md shadow-xl z-[9999]">
                    {items.map((item, i) =>
                        item.separator ? (
                            <div key={i} className="my-1.5 mx-3 border-t border-[var(--color-border)]" />
                        ) : (
                            <button
                                key={i}
                                onClick={() => { if (!item.disabled) { item.onClick(); onClose(); } }}
                                disabled={item.disabled}
                                className={`group w-full px-4 py-1.5 text-xs flex items-center justify-between select-none outline-none ${
                                    item.disabled
                                        ? 'text-[var(--color-text-tertiary)] cursor-default opacity-50'
                                        : 'text-[var(--color-text-primary)] hover:bg-[var(--color-accent)] hover:text-white cursor-default'
                                }`}
                            >
                                <span>{item.label}</span>
                                {item.shortcut && (
                                    <span className={`ml-8 text-[10px] ${item.disabled ? 'text-[var(--color-text-tertiary)]' : 'text-[var(--color-text-tertiary)] group-hover:text-white/70'}`}>{item.shortcut}</span>
                                )}
                            </button>
                        )
                    )}
                </div>
            )}
        </div>
    );
};

// ─── Cyber Shield SVG (inline, no import needed) ───────────────────────────

const CyberShieldIcon: React.FC<{ size?: number; className?: string }> = ({ size = 14, className }) => (
    <svg viewBox="0 0 120 120" width={size} height={size} fill="currentColor" className={className}>
        <path d="M126.3,13.2C97.8,18 78.1,45.1 82.4,73.6c1.1,7.3 4.4,16.1 8.1,21.5l1,1.4-39.9,39.9c-21.9,22-40.3,40.7-40.7,41.5-0.5,1-0.8,2.6-0.8,4.4 0,7.9 8.3,12.5 15,8.1l2-1.3 9,8.9c8,7.9 9.2,8.9 11.2,9.4 1.2,0.3 2.5,0.6 3,0.6 3.2,0 7.2-2.7 8.7-5.8 0.9-2 1-6 0.1-8.1-0.4-0.9-2.4-3.4-4.5-5.7l-3.9-4 5.6-5.6 5.6-5.6 3.8,3.8c5.5,5.4 7.9,6.4 12.5,5 6.2-1.8 8.9-9.2 5.4-14.8-0.5-0.7-4.5-5-9-9.5l-8.1-8.1 18.6-18.7c17.6-17.6 18.7-18.9 19.8-21.6 1.8-4.4 3.9-7.8 7.3-11.3l3-3.2-4.2-4.2c-4.8-4.8-7.2-8.5-8.9-13.9-3-9.6-1.9-20 3.1-28.5 2.8-4.8 8.4-10.3 13.1-12.6 6.3-3.2 9.7-4.1 16.6-4.1 5,0 6.6,0.2 9.5,1.1 4.7,1.5 9.9,4.2 12.8,6.8l2.4,2.1 2.7-0.7c4.2-1.1 11.4-1.7 15.5-1.4 3.1,0.3 3.6,0.2 3.3-0.3-3-5.1-9-11.9-13.6-15.4-6.4-4.9-16.2-9.2-24-10.4-3.4-0.7-13.9-0.7-17.2-0.1z" transform="matrix(0.509,0,0,0.509,-5.137,-5.118)"/>
        <path d="M167.1,54.2c-15.1,3.4-25.7,12.2-29.9,24.7-1.2,3.6-1.2,3.9-1.4,19.5l-0.2,15.8h-4.9c-7.6,0-12.4,1.6-16.9,5.7-2.9,2.6-5.3,6.6-6.3,10.4-0.7,2.7-0.8,5.8-0.8,24.7 0,22.9 0.2,27.7 2.1,35.4 5.4,23 23,42.1 45.6,49.7 8.3,2.8 11.4,3.2 22,3.2 10.5,0 13.7-0.5 22-3.2 11-3.6 20.3-9.6 28.6-18.4 9-9.3 14.9-20.9 17.8-34.4 0.9-4.1 1-6.4 1.2-28.5 0.1-16.2 0-24.9-0.3-26.8-0.8-4.2-2.9-8.2-6-11.2-4.7-4.7-9.6-6.5-17.7-6.5h-5l-0.1-16.1c-0.1-16-0.1-16-1.4-19.8C211.8,67.2 201.2,58.1 187.9,54.6 182.3,53.2 172.6,53 167.1,54.2zM184.8,74.1c6,1.8 11.1,6.4 12.4,11 0.3,1 0.5,7.5 0.5,15.4v13.8h-21-21V99.7c0-16.6 0-16.3 3.7-20.3 2.7-2.8 6.8-5.1 11.2-6.1 3.7-0.8 10.2-0.5 14.2,0.8zM181.7,152.9c2.4,1.2 5.4,4.6 6.2,6.8 0.8,2.5 0.7,6.1-0.2,8.6-0.8,2-4.8,6.4-5.9,6.4-0.8,0-0.5,1.2 2.8,9 1.8,4.3 3.2,8.1 3.2,8.6 0,3.1-1.6,3.6-11.3,3.6h-8.2l-1.3-1.3c-0.7-0.7-1.3-1.7-1.3-2.1 0-0.4 1.4-4.4 3.1-9l3.2-8.3-1.6-0.9c-3.6-1.9-6.1-6.2-6.1-10.6 0-5.5 3.4-10.1 8.8-11.8 1.9-0.5 6.6,0 8.6,1z" transform="matrix(0.509,0,0,0.509,-5.137,-5.118)"/>
    </svg>
);

// ─── Main Titlebar Component ────────────────────────────────────────────────

export const CustomTitlebar: React.FC<TitlebarProps> = (props) => {
    const {
        appTheme, theme, setTheme,
        isConnected, onDisconnect, onShowConnectionScreen, showConnectionScreen,
        onOpenSettings, onShowSupport, onShowCyberTools, onShowVault,
        onShowAbout, onShowShortcuts, onShowDependencies,
        masterPasswordSet, onLockApp, onSetupMasterPassword,
        onRefresh, onNewFolder, onToggleDevTools, onToggleTheme,
        onToggleDebugMode, onRename, onDelete, onSelectAll,
        onCut, onCopy, onPaste, hasSelection, hasClipboard,
        onToggleEditor, onToggleTerminal, onToggleAgent, onQuit,
        hasActivity,
    } = props;

    const t = useTranslation();
    const [isMaximized, setIsMaximized] = React.useState(false);
    const [openMenu, setOpenMenu] = React.useState<string | null>(null);

    React.useEffect(() => {
        const updateMaximized = async () => {
            try {
                setIsMaximized(await getCurrentWindow().isMaximized());
            } catch { /* ignore */ }
        };
        updateMaximized();
        const unlisten = getCurrentWindow().onResized(updateMaximized);
        return () => { unlisten.then(fn => fn()); };
    }, []);

    const handleMinimize = async (e: React.MouseEvent) => {
        e.stopPropagation(); e.preventDefault();
        await getCurrentWindow().minimize();
    };
    const handleMaximize = async (e: React.MouseEvent) => {
        e.stopPropagation(); e.preventDefault();
        await getCurrentWindow().toggleMaximize();
    };
    const handleClose = async (e: React.MouseEvent) => {
        e.stopPropagation(); e.preventDefault();
        await getCurrentWindow().close();
    };

    // ─── Menu definitions ───────────────────────────────────────────

    // File panel is visible when connected or in AeroFile mode (not on connection screen)
    const hasFilePanel = isConnected || !showConnectionScreen;

    const fileMenu: MenuEntry[] = [
        { label: t('menu.newFolder'), shortcut: 'Ctrl+N', onClick: onNewFolder, disabled: !hasFilePanel },
        { label: t('common.settings'), shortcut: 'Ctrl+,', onClick: onOpenSettings },
        { separator: true },
        { label: t('menu.debugMode'), shortcut: 'Ctrl+Shift+F12', onClick: onToggleDebugMode },
        { label: t('menu.dependencies'), onClick: onShowDependencies },
        { separator: true },
        { label: t('menu.quit'), shortcut: 'Ctrl+Q', onClick: onQuit },
    ];

    const editMenu: MenuEntry[] = [
        { label: t('menu.cut'), shortcut: 'Ctrl+X', onClick: onCut, disabled: !hasFilePanel || !hasSelection },
        { label: t('menu.copy'), shortcut: 'Ctrl+C', onClick: onCopy, disabled: !hasFilePanel || !hasSelection },
        { label: t('menu.paste'), shortcut: 'Ctrl+V', onClick: onPaste, disabled: !hasFilePanel || !hasClipboard },
        { separator: true },
        { label: t('menu.rename'), shortcut: 'F2', onClick: onRename, disabled: !hasFilePanel || !hasSelection },
        { label: t('menu.delete'), shortcut: 'Del', onClick: onDelete, disabled: !hasFilePanel || !hasSelection },
        { separator: true },
        { label: t('menu.selectAll'), shortcut: 'Ctrl+A', onClick: onSelectAll, disabled: !hasFilePanel },
    ];

    const viewMenu: MenuEntry[] = [
        { label: t('menu.refresh'), shortcut: 'Ctrl+R', onClick: onRefresh, disabled: !hasFilePanel },
        { separator: true },
        { label: t('menu.toggleTheme'), shortcut: 'Ctrl+T', onClick: onToggleTheme },
        { separator: true },
        { label: t('menu.toggleDevtools'), shortcut: 'Ctrl+Shift+D', onClick: onToggleDevTools },
        { label: t('menu.toggleEditor'), shortcut: 'Ctrl+1', onClick: onToggleEditor },
        { label: t('menu.toggleTerminal'), shortcut: 'Ctrl+2', onClick: onToggleTerminal },
        { label: t('menu.toggleAgent'), shortcut: 'Ctrl+3', onClick: onToggleAgent },
    ];

    const helpMenu: MenuEntry[] = [
        { label: t('menu.shortcuts'), shortcut: 'F1', onClick: onShowShortcuts },
        { separator: true },
        { label: t('menu.support'), onClick: onShowSupport },
        { label: t('menu.about'), onClick: onShowAbout },
    ];

    const closeMenu = React.useCallback(() => setOpenMenu(null), []);
    const anyMenuOpen = openMenu !== null;

    // Logo icon per theme
    const iconSrc = appTheme === 'light' ? '/icons/AeroFTP_simbol_color_light_120x120.png'
        : appTheme === 'tokyo' ? '/icons/AeroFTP_simbol_color_tokio_120x120.png'
        : appTheme === 'cyber' ? '/icons/AeroFTP_simbol_color_cyber_120x120.png'
        : '/icons/AeroFTP_simbol_color_dark_120x120.png';

    const activityCls = hasActivity ? 'animate-pulse' : '';

    return (
        <div
            data-tauri-drag-region
            className="flex items-center h-9 px-2 bg-[var(--color-bg-secondary)] border-b border-[var(--color-border)] select-none shrink-0"
        >
            {/* Left: Logo + App name */}
            <div
                data-tauri-drag-region
                className="flex items-center gap-1.5 px-1.5 h-full pointer-events-none shrink-0"
            >
                <img
                    src={iconSrc}
                    alt="AeroFTP" width={18} height={18}
                    className={`shrink-0 object-contain ${activityCls}`}
                />
                <span className="text-xs font-bold tracking-tight text-[var(--color-text-primary)]">
                    AeroFTP
                </span>
            </div>

            {/* Menus */}
            <div className="flex items-center">
                <TitlebarMenu
                    label={t('menu.file')}
                    items={fileMenu}
                    isOpen={openMenu === 'file'}
                    onOpen={() => setOpenMenu('file')}
                    onClose={closeMenu}
                    anyMenuOpen={anyMenuOpen}
                />
                <TitlebarMenu
                    label={t('menu.edit')}
                    items={editMenu}
                    isOpen={openMenu === 'edit'}
                    onOpen={() => setOpenMenu('edit')}
                    onClose={closeMenu}
                    anyMenuOpen={anyMenuOpen}
                />
                <TitlebarMenu
                    label={t('menu.view')}
                    items={viewMenu}
                    isOpen={openMenu === 'view'}
                    onOpen={() => setOpenMenu('view')}
                    onClose={closeMenu}
                    anyMenuOpen={anyMenuOpen}
                />
                <TitlebarMenu
                    label={t('menu.help')}
                    items={helpMenu}
                    isOpen={openMenu === 'help'}
                    onOpen={() => setOpenMenu('help')}
                    onClose={closeMenu}
                    anyMenuOpen={anyMenuOpen}
                />
            </div>

            {/* Center: drag region spacer */}
            <div data-tauri-drag-region className="flex-1 h-full" />

            {/* Right: Toolbar buttons + Window controls */}
            <div className="flex items-center gap-0.5">
                {/* Support */}
                <button
                    onClick={onShowSupport}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                    title={t('about.supportDev')}
                >
                    <Heart size={14} className="text-blue-500 fill-current" />
                </button>

                {/* Settings */}
                <button
                    onClick={onOpenSettings}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                    title={t('common.settings')}
                >
                    <Settings size={14} className="text-[var(--color-text-secondary)]" />
                </button>

                {/* Theme Toggle — has visible bg except in auto */}
                <ThemeToggle theme={theme} setTheme={setTheme} />

                <div className="w-px h-4 bg-[var(--color-border)] mx-1" />

                {/* AeroVault */}
                <button
                    onClick={onShowVault}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                    title={t('vault.titleFull')}
                >
                    <VaultIcon size={14} className="text-emerald-500" />
                </button>

                {/* Cyber Toolkit — cyber theme only, next to Vault */}
                {appTheme === 'cyber' && (
                    <button
                        onClick={onShowCyberTools}
                        className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                        title={t('cyberTools.title')}
                    >
                        <CyberShieldIcon size={14} className="text-emerald-400" />
                    </button>
                )}

                {/* Lock / Master Password */}
                <button
                    onClick={masterPasswordSet ? onLockApp : onSetupMasterPassword}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                    title={masterPasswordSet ? t('masterPassword.lockTooltip') : t('masterPassword.setupTooltip')}
                >
                    {masterPasswordSet ? (
                        <Lock size={14} className="text-emerald-500" />
                    ) : (
                        <LockOpen size={14} className="text-[var(--color-text-tertiary)]" />
                    )}
                </button>

                {/* Connect / Disconnect */}
                {isConnected ? (
                    <button
                        onClick={onDisconnect}
                        className="h-6 px-2 ml-1 flex items-center gap-1.5 text-[11px] rounded-md bg-red-500 hover:bg-red-600 text-white transition-colors cursor-pointer"
                        title={t('common.disconnect')}
                    >
                        <LogOut size={11} />
                        <span>{t('common.disconnect')}</span>
                    </button>
                ) : !showConnectionScreen && (
                    <button
                        onClick={onShowConnectionScreen}
                        className="h-6 px-2 ml-1 flex items-center gap-1.5 text-[11px] rounded-md bg-blue-500 hover:bg-blue-600 text-white transition-colors cursor-pointer"
                        title={t('common.connect')}
                    >
                        <Cloud size={11} />
                        <span>{t('common.connect')}</span>
                    </button>
                )}

                <div className="w-px h-4 bg-[var(--color-border)] mx-1" />

                {/* Window controls */}
                <button
                    onClick={handleMinimize}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                    title={t('ui.minimize')}
                >
                    <Minus size={14} className="text-[var(--color-text-secondary)]" />
                </button>
                <button
                    onClick={handleMaximize}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-[var(--color-bg-tertiary)] transition-colors cursor-pointer"
                    title={isMaximized ? t('ui.restore') : t('ui.maximize')}
                >
                    {isMaximized ? (
                        <Square size={11} className="text-[var(--color-text-secondary)]" />
                    ) : (
                        <Maximize2 size={14} className="text-[var(--color-text-secondary)]" />
                    )}
                </button>
                <button
                    onClick={handleClose}
                    className="w-7 h-7 flex items-center justify-center rounded-md hover:bg-red-500/90 transition-colors cursor-pointer group"
                    title={t('ui.close')}
                >
                    <X size={15} className="text-[var(--color-text-secondary)] group-hover:text-white" />
                </button>
            </div>
        </div>
    );
};

export default CustomTitlebar;
