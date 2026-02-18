import React, { useState, useEffect, useCallback } from 'react';
import { Trash2, Database, Search, Calendar, X, AlertTriangle } from 'lucide-react';
import { getChatStats, clearAllHistory, cleanupHistory, searchHistory, type ChatStats, type SearchResult } from '../../utils/chatHistory';
import { useTranslation } from '../../i18n';

interface ChatHistoryManagerProps {
    visible: boolean;
    onClose: () => void;
    onSessionDeleted: () => void;
    onNavigateToSession: (sessionId: string) => void;
}

export const ChatHistoryManager: React.FC<ChatHistoryManagerProps> = ({
    visible,
    onClose,
    onSessionDeleted,
    onNavigateToSession,
}) => {
    const t = useTranslation();
    const [stats, setStats] = useState<ChatStats | null>(null);
    const [searchQuery, setSearchQuery] = useState('');
    const [searchResults, setSearchResults] = useState<SearchResult[]>([]);
    const [searching, setSearching] = useState(false);
    const [showCleanup, setShowCleanup] = useState(false);
    const [cleanupDays, setCleanupDays] = useState(90);
    const [confirmClearAll, setConfirmClearAll] = useState(false);

    // Load stats on open
    useEffect(() => {
        if (visible) {
            getChatStats().then(s => setStats(s));
        }
    }, [visible]);

    // UX-003: Escape key to close
    useEffect(() => {
        if (!visible) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [visible, onClose]);

    // FTS5 search
    const handleSearch = useCallback(async () => {
        if (searchQuery.trim().length < 2) {
            setSearchResults([]);
            return;
        }
        setSearching(true);
        const results = await searchHistory(searchQuery.trim(), 30);
        setSearchResults(results);
        setSearching(false);
    }, [searchQuery]);

    useEffect(() => {
        const timer = setTimeout(() => {
            if (searchQuery.length >= 2) handleSearch();
            else setSearchResults([]);
        }, 300);
        return () => clearTimeout(timer);
    }, [searchQuery, handleSearch]);

    // Cleanup old sessions
    const handleCleanup = useCallback(async () => {
        const deleted = await cleanupHistory(cleanupDays);
        if (deleted > 0) {
            onSessionDeleted();
            getChatStats().then(s => setStats(s));
        }
        setShowCleanup(false);
    }, [cleanupDays, onSessionDeleted]);

    // F4: Clear all — dedicated command instead of semantic overload
    const handleClearAll = useCallback(async () => {
        await clearAllHistory();
        setConfirmClearAll(false);
        onSessionDeleted();
        getChatStats().then(s => setStats(s));
    }, [onSessionDeleted]);

    const formatBytes = (bytes: number): string => {
        if (bytes < 1024) return `${bytes} B`;
        if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
        return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
    };

    if (!visible) return null;

    return (
        // UX-004: ARIA dialog attributes
        <div
            role="dialog"
            aria-modal="true"
            aria-label={t('ai.history.manager')}
            className="absolute inset-0 z-50 flex flex-col bg-[var(--color-bg-primary)] text-[var(--color-text-primary)]"
        >
            {/* Header */}
            <div className="flex items-center justify-between px-3 py-2 border-b border-[var(--color-border)]">
                <div className="flex items-center gap-2">
                    <Database size={14} className="text-purple-400" />
                    <span className="text-xs font-medium">{t('ai.history.manager')}</span>
                </div>
                <button
                    onClick={onClose}
                    aria-label="Close"
                    className="p-1 rounded hover:bg-[var(--color-bg-tertiary)] transition-colors"
                >
                    <X size={14} />
                </button>
            </div>

            {/* Stats bar — UX-005: flex-wrap for narrow panels */}
            {stats && (
                <div className="flex items-center gap-2 flex-wrap px-3 py-1.5 text-[10px] text-[var(--color-text-secondary)] border-b border-[var(--color-border)] bg-[var(--color-bg-secondary)]">
                    <span>{t('ai.history.sessions')}: <strong>{stats.total_sessions}</strong></span>
                    <span>{t('ai.history.messagesCount')}: <strong>{stats.total_messages}</strong></span>
                    <span>{t('ai.history.totalTokens')}: <strong>{stats.total_tokens.toLocaleString()}</strong></span>
                    <span>{t('ai.history.totalCost')}: <strong>{new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD' }).format(stats.total_cost)}</strong></span>
                    <span>{t('ai.history.dbSize')}: <strong>{formatBytes(stats.db_size_bytes)}</strong></span>
                </div>
            )}

            {/* Search bar */}
            <div className="flex items-center gap-2 px-3 py-1.5 border-b border-[var(--color-border)]">
                <Search size={13} className="text-[var(--color-text-secondary)] shrink-0 opacity-60" />
                <input
                    type="text"
                    value={searchQuery}
                    onChange={e => setSearchQuery(e.target.value)}
                    placeholder={t('ai.history.searchPlaceholder')}
                    className="flex-1 bg-transparent text-xs text-[var(--color-text-primary)] placeholder-[var(--color-text-secondary)] focus:outline-none"
                    autoComplete="off"
                    autoFocus
                />
                {searching && (
                    <span className="text-[10px] text-[var(--color-text-secondary)]" aria-live="polite">{t('ai.history.searching')}</span>
                )}
            </div>

            {/* Search results */}
            <div className="flex-1 overflow-y-auto">
                {searchResults.length > 0 ? (
                    <div className="divide-y divide-[var(--color-border)]">
                        {searchResults.map(result => (
                            <button
                                key={result.message_id}
                                onClick={() => {
                                    onNavigateToSession(result.session_id);
                                    onClose();
                                }}
                                className="w-full text-left px-3 py-2 hover:bg-[var(--color-bg-tertiary)] transition-colors"
                            >
                                <div className="flex items-center gap-2 mb-0.5">
                                    {/* I18N-001: Use translated role labels; THEME-003: theme-compatible colors */}
                                    <span className={`text-[9px] px-1 py-0.5 rounded ${
                                        result.role === 'user'
                                            ? 'bg-blue-500/20 text-blue-400 dark:text-blue-300'
                                            : 'bg-purple-500/20 text-purple-400 dark:text-purple-300'
                                    }`}>
                                        {result.role === 'user' ? t('ai.history.roleUser') : t('ai.history.roleAI')}
                                    </span>
                                    <span className="text-[10px] text-[var(--color-text-secondary)] truncate">{result.session_title}</span>
                                    <span className="text-[9px] text-[var(--color-text-secondary)] opacity-60 ml-auto shrink-0">
                                        {new Date(result.created_at).toLocaleDateString()}
                                    </span>
                                </div>
                                {/* SEC-001/UX-001: Snippet is pre-sanitized by Rust backend */}
                                <p
                                    className="text-[11px] text-[var(--color-text-primary)] line-clamp-2 [&_mark]:bg-purple-500/30 [&_mark]:text-inherit [&_mark]:rounded-sm"
                                    dangerouslySetInnerHTML={{ __html: result.snippet }}
                                />
                            </button>
                        ))}
                    </div>
                ) : searchQuery.length >= 2 && !searching ? (
                    <div className="flex items-center justify-center h-32 text-xs text-[var(--color-text-secondary)] opacity-60">
                        {t('ai.history.noResults')}
                    </div>
                ) : (
                    <div className="flex flex-col items-center justify-center h-32 gap-2 text-[var(--color-text-secondary)] opacity-60">
                        <Search size={24} className="opacity-30" />
                        <span className="text-xs">{t('ai.history.searchHint')}</span>
                    </div>
                )}
            </div>

            {/* Actions bar */}
            <div className="flex items-center gap-2 px-3 py-2 border-t border-[var(--color-border)] bg-[var(--color-bg-secondary)]">
                {showCleanup ? (
                    <div className="flex items-center gap-2 flex-1">
                        <Calendar size={12} className="text-yellow-400 shrink-0" />
                        <span className="text-[10px]">{t('ai.history.deleteOlderThan')}</span>
                        <select
                            value={cleanupDays}
                            onChange={e => setCleanupDays(Number(e.target.value))}
                            className="text-[10px] bg-[var(--color-bg-tertiary)] border border-[var(--color-border)] rounded px-1 py-0.5 text-[var(--color-text-primary)]"
                        >
                            <option value={30}>30 {t('ai.history.days')}</option>
                            <option value={60}>60 {t('ai.history.days')}</option>
                            <option value={90}>90 {t('ai.history.days')}</option>
                            <option value={180}>180 {t('ai.history.days')}</option>
                            <option value={365}>365 {t('ai.history.days')}</option>
                        </select>
                        <button
                            onClick={handleCleanup}
                            className="text-[10px] px-2 py-0.5 rounded bg-yellow-600/20 text-yellow-300 hover:bg-yellow-600/30 transition-colors"
                        >
                            {t('ai.history.cleanup')}
                        </button>
                        <button
                            onClick={() => setShowCleanup(false)}
                            className="text-[10px] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)]"
                        >
                            {t('ai.history.cancel')}
                        </button>
                    </div>
                ) : confirmClearAll ? (
                    <div className="flex items-center gap-2 flex-1">
                        <AlertTriangle size={12} className="text-red-400 shrink-0" />
                        <span className="text-[10px] text-red-300">{t('ai.history.confirmClearAll')}</span>
                        <button
                            onClick={handleClearAll}
                            className="text-[10px] px-2 py-0.5 rounded bg-red-600/20 text-red-300 hover:bg-red-600/30 transition-colors"
                        >
                            {t('ai.history.confirmDelete')}
                        </button>
                        <button
                            onClick={() => setConfirmClearAll(false)}
                            className="text-[10px] text-[var(--color-text-secondary)] hover:text-[var(--color-text-primary)]"
                        >
                            {t('ai.history.cancel')}
                        </button>
                    </div>
                ) : (
                    <>
                        <button
                            onClick={() => setShowCleanup(true)}
                            className="flex items-center gap-1 text-[10px] px-2 py-1 rounded hover:bg-[var(--color-bg-tertiary)] text-[var(--color-text-secondary)] transition-colors"
                        >
                            <Calendar size={11} />
                            {t('ai.history.cleanupOld')}
                        </button>
                        <button
                            onClick={() => setConfirmClearAll(true)}
                            className="flex items-center gap-1 text-[10px] px-2 py-1 rounded hover:bg-red-600/10 text-red-400 transition-colors"
                        >
                            <Trash2 size={11} />
                            {t('ai.history.clearAll')}
                        </button>
                    </>
                )}
            </div>
        </div>
    );
};

export default ChatHistoryManager;
