import React, { useMemo } from 'react';
import { Sparkles, PanelLeftClose, PanelLeftOpen, Plus, Download, Settings2, Zap, ShieldCheck, Database } from 'lucide-react';
import { useTranslation } from '../../i18n';
import type { EffectiveTheme } from '../../hooks/useTheme';
import type { AgentMode } from './aiChatTypes';

interface AIChatHeaderProps {
    showHistory: boolean;
    onToggleHistory: () => void;
    onNewChat: () => void;
    showExportMenu: boolean;
    onToggleExportMenu: () => void;
    onExport: (format: 'markdown' | 'json') => void;
    onOpenSettings: () => void;
    onOpenHistoryManager: () => void;
    hasMessages: boolean;
    appTheme?: EffectiveTheme;
    agentMode?: AgentMode;
    onSetAgentMode?: (mode: AgentMode) => void;
    onExtremeWarning?: () => void;
}

export const AIChatHeader: React.FC<AIChatHeaderProps> = ({
    showHistory, onToggleHistory, onNewChat,
    showExportMenu, onToggleExportMenu, onExport,
    onOpenSettings, onOpenHistoryManager, hasMessages, appTheme = 'dark',
    agentMode = 'normal', onSetAgentMode, onExtremeWarning,
}) => {
    const t = useTranslation();

    const styles = useMemo(() => {
        switch (appTheme) {
            case 'light': return {
                headerBg: 'bg-gray-100/50 border-gray-300',
                textLabel: 'text-gray-700',
                btn: 'text-gray-500 hover:text-gray-900 hover:bg-gray-200',
                dropdown: 'bg-white border-gray-300 shadow-lg',
                dropdownItem: 'hover:bg-gray-100',
                sparkle: 'text-purple-500',
            };
            case 'tokyo': return {
                headerBg: 'bg-[#16161e]/50 border-[#292e42]',
                textLabel: 'text-[#a9b1d6]',
                btn: 'text-[#565f89] hover:text-[#c0caf5] hover:bg-[#292e42]',
                dropdown: 'bg-[#16161e] border-[#292e42] shadow-xl',
                dropdownItem: 'hover:bg-[#292e42]',
                sparkle: 'text-[#bb9af7]',
            };
            case 'cyber': return {
                headerBg: 'bg-[#0d1117]/50 border-emerald-900/40',
                textLabel: 'text-emerald-300',
                btn: 'text-gray-500 hover:text-emerald-300 hover:bg-emerald-500/10',
                dropdown: 'bg-[#0d1117] border-emerald-800/50 shadow-xl shadow-emerald-900/20',
                dropdownItem: 'hover:bg-emerald-500/10',
                sparkle: 'text-emerald-400',
            };
            default: return { // dark
                headerBg: 'bg-gray-800/50 border-gray-700/50',
                textLabel: 'text-gray-300',
                btn: 'text-gray-400 hover:text-white hover:bg-gray-700',
                dropdown: 'bg-gray-800 border-gray-600 shadow-xl',
                dropdownItem: 'hover:bg-gray-700',
                sparkle: 'text-purple-400',
            };
        }
    }, [appTheme]);

    const modeBadge = useMemo(() => {
        switch (agentMode) {
            case 'safe':
                return (
                    <button
                        onClick={onOpenSettings}
                        className="flex items-center gap-1 px-2 py-1 mr-1 rounded border border-teal-500/40 bg-teal-500/10 text-teal-400 text-[10px] font-bold cursor-pointer transition-all hover:bg-teal-500/20 hover:border-teal-500/60"
                        title={t('ai.agentMode.safe')}
                    >
                        <ShieldCheck size={10} className="shrink-0" />
                        <span className="tracking-wider uppercase">Safe</span>
                    </button>
                );
            case 'expert':
                return (
                    <button
                        onClick={onOpenSettings}
                        className="expert-active-bg flex items-center gap-1 px-2 py-1 mr-1 rounded border border-amber-500/40 text-amber-400 text-[10px] font-bold cursor-pointer transition-all hover:border-amber-500/60 shadow-[0_0_8px_rgba(245,158,11,0.15)]"
                        title={t('ai.agentMode.expert')}
                    >
                        <Zap size={10} className="shrink-0" />
                        <span className="tracking-wider uppercase">Expert</span>
                    </button>
                );
            case 'extreme':
                if (appTheme !== 'cyber') return null;
                return (
                    <button
                        onClick={onExtremeWarning}
                        className="extreme-active-bg flex items-center gap-1 px-2 py-1 mr-1 rounded border border-red-500/40 text-red-400 text-[10px] font-bold cursor-pointer transition-all hover:border-red-500/60 shadow-[0_0_8px_rgba(239,68,68,0.15)]"
                        title={t('ai.extremeMode.title')}
                    >
                        <Zap size={10} className="shrink-0" />
                        <span className="tracking-wider uppercase">Extreme</span>
                    </button>
                );
            default: // normal — no badge
                return null;
        }
    }, [agentMode, appTheme, onOpenSettings, onExtremeWarning, t]);

    return (
        <div className={`flex items-center justify-between px-4 py-2 ${styles.headerBg} border-b`}>
            {/* left side */}
            <div className={`flex items-center gap-2 text-sm ${styles.textLabel}`}>
                <button onClick={onToggleHistory} className={`p-1 ${styles.btn} rounded transition-colors`} title={showHistory ? t('ai.hideHistory') : t('ai.chatHistory')}>
                    {showHistory ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
                </button>
                <Sparkles size={14} className={styles.sparkle} />
                <span className="font-medium">{t('ai.aeroAgent')}</span>
            </div>
            {/* right side */}
            <div className="flex items-center gap-1">
                {modeBadge}
                <button onClick={onOpenHistoryManager} className={`p-1.5 ${styles.btn} rounded transition-colors`} title={t('ai.history.manager')}>
                    <Database size={14} />
                </button>
                <button onClick={onNewChat} className={`p-1.5 ${styles.btn} rounded transition-colors`} title={t('ai.newChat')}>
                    <Plus size={14} />
                </button>
                <div className="relative">
                    <button onClick={onToggleExportMenu} disabled={!hasMessages} className={`p-1.5 ${styles.btn} rounded transition-colors disabled:opacity-30 disabled:cursor-not-allowed`} title={t('ai.exportChat')}>
                        <Download size={14} />
                    </button>
                    {showExportMenu && (
                        <div className={`absolute right-0 top-full mt-1 ${styles.dropdown} border rounded-lg z-20 py-1 min-w-[180px]`}>
                            <button onClick={() => onExport('markdown')} className={`w-full px-3 py-2 text-left text-xs ${styles.dropdownItem} flex items-center gap-2`}>
                                <span>{t('ai.exportMarkdown')}</span>
                            </button>
                            <button onClick={() => onExport('json')} className={`w-full px-3 py-2 text-left text-xs ${styles.dropdownItem} flex items-center gap-2`}>
                                <span>{t('ai.exportJSON')}</span>
                            </button>
                        </div>
                    )}
                </div>
                <button onClick={onOpenSettings} className={`p-1.5 ${styles.btn} rounded transition-colors`} title={t('ai.aiSettings')}>
                    <Settings2 size={14} />
                </button>
            </div>
        </div>
    );
};
