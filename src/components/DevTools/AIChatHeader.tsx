import React from 'react';
import { Sparkles, PanelLeftClose, PanelLeftOpen, Plus, Download, Settings2 } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface AIChatHeaderProps {
    showHistory: boolean;
    onToggleHistory: () => void;
    onNewChat: () => void;
    showExportMenu: boolean;
    onToggleExportMenu: () => void;
    onExport: (format: 'markdown' | 'json') => void;
    onOpenSettings: () => void;
    hasMessages: boolean;
}

export const AIChatHeader: React.FC<AIChatHeaderProps> = ({
    showHistory, onToggleHistory, onNewChat,
    showExportMenu, onToggleExportMenu, onExport,
    onOpenSettings, hasMessages,
}) => {
    const t = useTranslation();
    return (
        <div className="flex items-center justify-between px-4 py-2 bg-gray-800/50 border-b border-gray-700/50">
            {/* left side */}
            <div className="flex items-center gap-2 text-sm text-gray-300">
                <button onClick={onToggleHistory} className="p-1 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors" title={showHistory ? t('ai.hideHistory') : t('ai.chatHistory')}>
                    {showHistory ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
                </button>
                <Sparkles size={14} className="text-purple-400" />
                <span className="font-medium">{t('ai.aeroAgent')}</span>
            </div>
            {/* right side */}
            <div className="flex items-center gap-1">
                <button onClick={onNewChat} className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors" title={t('ai.newChat')}>
                    <Plus size={14} />
                </button>
                <div className="relative">
                    <button onClick={onToggleExportMenu} disabled={!hasMessages} className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors disabled:opacity-30 disabled:cursor-not-allowed" title={t('ai.exportChat')}>
                        <Download size={14} />
                    </button>
                    {showExportMenu && (
                        <div className="absolute right-0 top-full mt-1 bg-gray-800 border border-gray-600 rounded-lg shadow-xl z-20 py-1 min-w-[180px]">
                            <button onClick={() => onExport('markdown')} className="w-full px-3 py-2 text-left text-xs hover:bg-gray-700 flex items-center gap-2">
                                <span>{t('ai.exportMarkdown')}</span>
                            </button>
                            <button onClick={() => onExport('json')} className="w-full px-3 py-2 text-left text-xs hover:bg-gray-700 flex items-center gap-2">
                                <span>{t('ai.exportJSON')}</span>
                            </button>
                        </div>
                    )}
                </div>
                <button onClick={onOpenSettings} className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors" title={t('ai.aiSettings')}>
                    <Settings2 size={14} />
                </button>
            </div>
        </div>
    );
};
