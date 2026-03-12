import * as React from 'react';
import { X, Loader2 } from 'lucide-react';
import { VaultIcon } from './icons/VaultIcon';
import { useTranslation } from '../i18n';
import { useVaultState, VaultMode, securityLevels, IconProvider } from './vault/useVaultState';
import { VaultHome } from './vault/VaultHome';
import { VaultCreate } from './vault/VaultCreate';
import { VaultOpen } from './vault/VaultOpen';
import { VaultBrowse } from './vault/VaultBrowse';

interface VaultPanelProps {
    onClose: () => void;
    isConnected?: boolean;
    initialPath?: string;
    initialFiles?: string[];
    initialMode?: VaultMode;
    initialFolderPath?: string;
    iconProvider?: IconProvider;
}

export type { VaultMode } from './vault/useVaultState';

export const VaultPanel: React.FC<VaultPanelProps> = ({ onClose, isConnected = false, initialPath, initialFiles, initialMode, initialFolderPath, iconProvider }) => {
    const t = useTranslation();

    const state = useVaultState({
        initialMode,
        initialPath,
        initialFiles,
        initialFolderPath,
        isConnected,
        onClose,
    });

    const vaultName = state.vaultPath.split(/[\\/]/).pop() || 'Vault';
    const currentLevelConfig = state.vaultSecurity ? securityLevels[state.vaultSecurity.level] : null;
    const LevelIcon = currentLevelConfig?.icon || VaultIcon;

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60"
            role="dialog"
            aria-modal="true"
            aria-label="AeroVault"
            onClick={(e) => { if (e.target === e.currentTarget) onClose(); }}
        >
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-full max-w-[700px] max-h-[85vh] flex flex-col animate-scale-in">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <VaultIcon size={18} className="text-emerald-400" />
                        <span className="font-medium">
                            {state.mode === 'browse' ? vaultName : t('vault.title')}
                        </span>
                        {/* Security badge in browse mode */}
                        {state.mode === 'browse' && currentLevelConfig && (
                            <span className={`ml-2 px-2 py-0.5 rounded text-xs font-medium ${currentLevelConfig.bgColor} bg-opacity-20 ${currentLevelConfig.color}`}>
                                <LevelIcon size={10} className="inline mr-1" />
                                {currentLevelConfig.label}
                            </span>
                        )}
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded" title={t('common.close')}><X size={18} /></button>
                </div>

                {/* Error / Success */}
                {state.error && <div className="px-4 py-2 bg-red-100 dark:bg-red-900/30 text-red-600 dark:text-red-400 text-sm">{state.error}</div>}
                {state.success && <div className="px-4 py-2 bg-green-100 dark:bg-green-900/30 text-green-600 dark:text-green-400 text-sm">{state.success}</div>}

                {/* Content */}
                {state.mode === 'home' && <VaultHome state={state} isConnected={isConnected} iconProvider={iconProvider} />}
                {state.mode === 'create' && <VaultCreate state={state} />}
                {state.mode === 'open' && <VaultOpen state={state} />}
                {state.mode === 'browse' && <VaultBrowse state={state} iconProvider={iconProvider} />}

                {/* Loading overlay */}
                {state.loading && state.mode === 'browse' && (
                    <div className="absolute inset-0 bg-black/30 flex items-center justify-center rounded-lg">
                        <Loader2 size={24} className="animate-spin text-blue-400" />
                    </div>
                )}
            </div>
        </div>
    );
};
