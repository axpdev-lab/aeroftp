// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import React, { useMemo, useState, useRef, useEffect, useCallback } from 'react';
import { Sparkles, PanelLeftClose, PanelLeftOpen, Plus, Download, Settings2, Zap, ShieldCheck, Database, ChevronDown, Brain, Flame } from 'lucide-react';
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

/** Mode metadata: icon, color scheme, border/bg classes per mode */
const MODE_CONFIG: Record<AgentMode, {
    icon: React.ElementType;
    color: string;
    border: string;
    bg: string;
    bgHover: string;
    text: string;
    dot: string;
}> = {
    safe: {
        icon: ShieldCheck,
        color: 'teal',
        border: 'border-teal-500/40',
        bg: 'bg-teal-500/10',
        bgHover: 'hover:bg-teal-500/20',
        text: 'text-teal-400',
        dot: 'bg-teal-400',
    },
    normal: {
        icon: Brain,
        color: 'purple',
        border: 'border-purple-500/40',
        bg: 'bg-purple-500/10',
        bgHover: 'hover:bg-purple-500/20',
        text: 'text-purple-400',
        dot: 'bg-purple-400',
    },
    expert: {
        icon: Zap,
        color: 'amber',
        border: 'border-amber-500/40',
        bg: 'bg-amber-500/10',
        bgHover: 'hover:bg-amber-500/20',
        text: 'text-amber-400',
        dot: 'bg-amber-400',
    },
    extreme: {
        icon: Flame,
        color: 'red',
        border: 'border-red-500/40',
        bg: 'bg-red-500/10',
        bgHover: 'hover:bg-red-500/20',
        text: 'text-red-400',
        dot: 'bg-red-400',
    },
};

const MODES: AgentMode[] = ['safe', 'normal', 'expert', 'extreme'];

export const AIChatHeader: React.FC<AIChatHeaderProps> = ({
    showHistory, onToggleHistory, onNewChat,
    showExportMenu, onToggleExportMenu, onExport,
    onOpenSettings, onOpenHistoryManager, hasMessages, appTheme = 'dark',
    agentMode = 'normal', onSetAgentMode, onExtremeWarning,
}) => {
    const t = useTranslation();
    const [showModeMenu, setShowModeMenu] = useState(false);
    const modeMenuRef = useRef<HTMLDivElement>(null);

    // Close dropdown on outside click
    useEffect(() => {
        if (!showModeMenu) return;
        const handleClick = (e: MouseEvent) => {
            if (modeMenuRef.current && !modeMenuRef.current.contains(e.target as Node)) {
                setShowModeMenu(false);
            }
        };
        document.addEventListener('mousedown', handleClick);
        return () => document.removeEventListener('mousedown', handleClick);
    }, [showModeMenu]);

    const selectMode = useCallback((mode: AgentMode) => {
        if (mode === 'extreme' && appTheme !== 'cyber') {
            onExtremeWarning?.();
            setShowModeMenu(false);
            return;
        }
        onSetAgentMode?.(mode);
        setShowModeMenu(false);
    }, [appTheme, onSetAgentMode, onExtremeWarning]);

    const styles = useMemo(() => {
        switch (appTheme) {
            case 'light': return {
                headerBg: 'bg-gray-100/50 border-gray-300',
                textLabel: 'text-gray-700',
                btn: 'text-gray-500 hover:text-gray-900 hover:bg-gray-200',
                dropdown: 'bg-white border-gray-300 shadow-lg',
                dropdownItem: 'hover:bg-gray-100',
                dropdownItemText: 'text-gray-600',
                sparkle: 'text-purple-500',
            };
            case 'tokyo': return {
                headerBg: 'bg-[#16161e]/50 border-[#292e42]',
                textLabel: 'text-[#a9b1d6]',
                btn: 'text-[#565f89] hover:text-[#c0caf5] hover:bg-[#292e42]',
                dropdown: 'bg-[#16161e] border-[#292e42] shadow-xl',
                dropdownItem: 'hover:bg-[#292e42]',
                dropdownItemText: 'text-[#565f89]',
                sparkle: 'text-[#bb9af7]',
            };
            case 'cyber': return {
                headerBg: 'bg-[#0d1117]/50 border-emerald-900/40',
                textLabel: 'text-emerald-300',
                btn: 'text-gray-500 hover:text-emerald-300 hover:bg-emerald-500/10',
                dropdown: 'bg-[#0d1117] border-emerald-800/50 shadow-xl shadow-emerald-900/20',
                dropdownItem: 'hover:bg-emerald-500/10',
                dropdownItemText: 'text-gray-500',
                sparkle: 'text-emerald-400',
            };
            default: return { // dark
                headerBg: 'bg-gray-800/50 border-gray-700/50',
                textLabel: 'text-gray-300',
                btn: 'text-gray-400 hover:text-white hover:bg-gray-700',
                dropdown: 'bg-gray-800 border-gray-600 shadow-xl',
                dropdownItem: 'hover:bg-gray-700',
                dropdownItemText: 'text-gray-500',
                sparkle: 'text-purple-400',
            };
        }
    }, [appTheme]);

    const currentConfig = MODE_CONFIG[agentMode];
    const CurrentIcon = currentConfig.icon;

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
                {/* Agent Mode quick-switch dropdown */}
                <div className="relative" ref={modeMenuRef}>
                    <button
                        onClick={() => setShowModeMenu(prev => !prev)}
                        className={`flex items-center gap-1 px-2 py-1 mr-1 rounded border ${currentConfig.border} ${currentConfig.bg} ${currentConfig.text} text-[10px] font-bold cursor-pointer transition-all ${currentConfig.bgHover}`}
                        title={t('ai.agentMode.title')}
                    >
                        <CurrentIcon size={10} className="shrink-0" />
                        <span className="tracking-wider uppercase">{t(`ai.agentMode.${agentMode}`)}</span>
                        <ChevronDown size={9} className={`shrink-0 transition-transform ${showModeMenu ? 'rotate-180' : ''}`} />
                    </button>
                    {showModeMenu && (
                        <div className={`absolute right-0 top-full mt-1 ${styles.dropdown} border rounded-lg z-30 py-1 min-w-[240px]`}>
                            {MODES.map(mode => {
                                const cfg = MODE_CONFIG[mode];
                                const Icon = cfg.icon;
                                const isActive = mode === agentMode;
                                const isLocked = mode === 'extreme' && appTheme !== 'cyber';
                                return (
                                    <button
                                        key={mode}
                                        onClick={() => selectMode(mode)}
                                        className={`w-full px-3 py-2.5 text-left text-xs ${styles.dropdownItem} flex items-start gap-2.5 transition-colors ${isActive ? cfg.bg : ''} ${isLocked ? 'opacity-50' : ''}`}
                                    >
                                        <div className={`mt-0.5 p-1 rounded ${isActive ? cfg.bg : ''}`}>
                                            <Icon size={13} className={isActive ? cfg.text : styles.dropdownItemText} />
                                        </div>
                                        <div className="flex-1 min-w-0">
                                            <div className="flex items-center gap-1.5">
                                                <span className={`font-semibold ${isActive ? cfg.text : styles.textLabel}`}>
                                                    {t(`ai.agentMode.${mode}`)}
                                                </span>
                                                {isActive && (
                                                    <span className={`w-1.5 h-1.5 rounded-full ${cfg.dot}`} />
                                                )}
                                            </div>
                                            <p className={`${styles.dropdownItemText} text-[10px] mt-0.5 leading-snug`}>
                                                {t(`ai.agentMode.${mode}Desc`)}
                                            </p>
                                        </div>
                                    </button>
                                );
                            })}
                        </div>
                    )}
                </div>
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
