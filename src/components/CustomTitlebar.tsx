import * as React from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Minus, Square, X, Maximize2, Settings } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { EffectiveTheme } from '../hooks/useTheme';

interface TitlebarProps {
    showMenuBar: boolean;
    onToggleMenuBar: () => void;
    onOpenSettings: () => void;
    appTheme?: EffectiveTheme;
}

export const CustomTitlebar: React.FC<TitlebarProps> = ({ showMenuBar, onToggleMenuBar, onOpenSettings, appTheme = 'dark' }) => {
    const t = useTranslation();
    const [isMaximized, setIsMaximized] = React.useState(false);

    React.useEffect(() => {
        const updateMaximized = async () => {
            const appWindow = getCurrentWindow();
            setIsMaximized(await appWindow.isMaximized());
        };
        updateMaximized();

        const unlisten = getCurrentWindow().onResized(updateMaximized);
        return () => { unlisten.then(fn => fn()); };
    }, []);

    const handleMinimize = async (e: React.MouseEvent) => {
        e.stopPropagation();
        await getCurrentWindow().minimize();
    };

    const handleMaximize = async (e: React.MouseEvent) => {
        e.stopPropagation();
        const appWindow = getCurrentWindow();
        if (isMaximized) await appWindow.unmaximize();
        else await appWindow.maximize();
        setIsMaximized(!isMaximized);
    };

    const handleClose = async (e: React.MouseEvent) => {
        e.stopPropagation();
        await getCurrentWindow().close();
    };

    // Programmatic Drag Handler for Linux Reliability
    const startDrag = (e: React.MouseEvent) => {
        // Only drag on left click and if not clicking a button
        if (e.button === 0) {
            getCurrentWindow().startDragging();
        }
    };

    return (
        <div
            className="flex items-center justify-between h-9 px-2 bg-gradient-to-r from-blue-600 via-blue-500 to-cyan-500 dark:from-gray-800 dark:via-gray-750 dark:to-gray-700 select-none"
            onMouseDown={startDrag} // Global drag handler for the bar background
        >
            {/* Left: Logo icon + App name (Pass-through drag) */}
            <div className="flex items-center gap-2 px-2 h-full cursor-default z-10 select-none pointer-events-none">
                <img
                    src={appTheme === 'light' ? '/icons/AeroFTP_simbol_color_light_120x120.png'
                        : appTheme === 'tokyo' ? '/icons/AeroFTP_simbol_color_tokio_120x120.png'
                        : appTheme === 'cyber' ? '/icons/AeroFTP_simbol_color_cyber_120x120.png'
                        : '/icons/AeroFTP_simbol_color_dark_120x120.png'}
                    alt="AeroFTP" width={20} height={20} className="shrink-0 object-contain"
                />
                <span className="text-sm font-bold tracking-tight">
                    <span className="text-white/90">Aero</span>
                    <span className="text-white/90">FTP</span>
                </span>
            </div>

            {/* Center Spacer with holes for buttons */}
            <div className="flex-1 h-full flex items-center justify-center overflow-hidden">
                <button
                    onMouseDown={(e) => e.stopPropagation()} // Stop drag propagation
                    onClick={onToggleMenuBar}
                    className="text-white/60 hover:text-white text-xs transition-colors hover:bg-white/10 px-2 py-0.5 rounded cursor-pointer shrink-0 mx-2 z-20"
                >
                    {showMenuBar ? t('ui.hideSystemMenu') : t('ui.showSystemMenu')}
                </button>
            </div>

            {/* Right: Window controls (Stop Propagation) */}
            <div className="flex items-center z-20" onMouseDown={(e) => e.stopPropagation()}>
                {/* Settings Button */}
                <button
                    onClick={onOpenSettings}
                    className="w-10 h-9 flex items-center justify-center hover:bg-white/20 transition-colors group cursor-pointer mr-1"
                    title={t('common.settings')}
                >
                    <Settings size={14} className="text-white/80 group-hover:text-white" />
                </button>

                <div className="w-px h-4 bg-white/20 mx-1" />

                <button
                    onClick={handleMinimize}
                    className="w-10 h-9 flex items-center justify-center hover:bg-white/20 transition-colors group cursor-pointer"
                    title={t('ui.minimize')}
                >
                    <Minus size={14} className="text-white/80 group-hover:text-white" />
                </button>
                <button
                    onClick={handleMaximize}
                    className="w-10 h-9 flex items-center justify-center hover:bg-white/20 transition-colors group cursor-pointer"
                    title={isMaximized ? t('ui.restore') : t('ui.maximize')}
                >
                    {isMaximized ? (
                        <Square size={12} className="text-white/80 group-hover:text-white" />
                    ) : (
                        <Maximize2 size={14} className="text-white/80 group-hover:text-white" />
                    )}
                </button>
                <button
                    onClick={handleClose}
                    className="w-10 h-9 flex items-center justify-center hover:bg-red-500 transition-colors group cursor-pointer"
                    title={t('ui.close')}
                >
                    <X size={16} className="text-white/80 group-hover:text-white" />
                </button>
            </div>
        </div>
    );
};

export default CustomTitlebar;
