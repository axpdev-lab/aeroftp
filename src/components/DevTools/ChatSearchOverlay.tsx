import React, { useState, useEffect, useRef, useCallback, useMemo, type MutableRefObject } from 'react';
import { Search, ChevronUp, ChevronDown, X } from 'lucide-react';
import { useTranslation } from '../../i18n';

/** A match reference pointing to a specific location within a chat message */
export interface SearchMatch {
    messageId: string;
    messageIndex: number;
    startOffset: number;
    matchLength: number;
}

interface ChatSearchOverlayProps {
    messages: Array<{
        id: string;
        role: 'user' | 'assistant';
        content: string;
    }>;
    visible: boolean;
    onClose: () => void;
    onHighlightMessage: (messageId: string) => void;
    onSearchResults: (matches: SearchMatch[], activeIndex: number) => void;
}

export const ChatSearchOverlay: React.FC<ChatSearchOverlayProps> = ({
    messages,
    visible,
    onClose,
    onHighlightMessage,
    onSearchResults,
}) => {
    const t = useTranslation();
    const [query, setQuery] = useState('');
    const [activeMatchIndex, setActiveMatchIndex] = useState(0);
    const [roleFilter, setRoleFilter] = useState<'all' | 'user' | 'assistant'>('all');
    const inputRef = useRef<HTMLInputElement>(null);
    const onSearchResultsRef = useRef(onSearchResults);
    onSearchResultsRef.current = onSearchResults;
    const onHighlightMessageRef = useRef(onHighlightMessage);
    onHighlightMessageRef.current = onHighlightMessage;

    // Auto-focus on show
    useEffect(() => {
        if (visible) {
            inputRef.current?.focus();
            inputRef.current?.select();
        }
    }, [visible]);

    // Reset state when overlay is hidden
    useEffect(() => {
        if (!visible) {
            setQuery('');
            setActiveMatchIndex(0);
            setRoleFilter('all');
        }
    }, [visible]);

    // Compute matches
    const matches = useMemo(() => {
        if (!query || query.length < 2) return [];
        const results: SearchMatch[] = [];
        const lowerQuery = query.toLowerCase();

        messages.forEach((msg, msgIdx) => {
            if (roleFilter !== 'all' && msg.role !== roleFilter) return;
            const content = msg.content.toLowerCase();
            let pos = 0;
            while ((pos = content.indexOf(lowerQuery, pos)) !== -1) {
                results.push({
                    messageId: msg.id,
                    messageIndex: msgIdx,
                    startOffset: pos,
                    matchLength: query.length,
                });
                pos += 1;
            }
        });
        return results;
    }, [query, messages, roleFilter]);

    // Notify parent of results
    useEffect(() => {
        onSearchResultsRef.current(matches, activeMatchIndex);
        if (matches.length > 0 && matches[activeMatchIndex]) {
            onHighlightMessageRef.current(matches[activeMatchIndex].messageId);
        }
    }, [matches, activeMatchIndex]);

    const goNext = useCallback(() => {
        if (matches.length === 0) return;
        setActiveMatchIndex(prev => (prev + 1) % matches.length);
    }, [matches.length]);

    const goPrev = useCallback(() => {
        if (matches.length === 0) return;
        setActiveMatchIndex(prev => (prev - 1 + matches.length) % matches.length);
    }, [matches.length]);

    // Keyboard handling
    const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
        if (e.key === 'Escape') {
            onClose();
            return;
        }
        if (e.key === 'Enter') {
            e.shiftKey ? goPrev() : goNext();
            return;
        }
        if (e.key === 'ArrowDown') {
            e.preventDefault();
            goNext();
            return;
        }
        if (e.key === 'ArrowUp') {
            e.preventDefault();
            goPrev();
            return;
        }
    }, [goNext, goPrev, onClose]);

    const handleQueryChange = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
        setQuery(e.target.value);
        setActiveMatchIndex(0);
    }, []);

    const handleRoleFilterChange = useCallback((role: 'all' | 'user' | 'assistant') => {
        setRoleFilter(role);
        setActiveMatchIndex(0);
    }, []);

    if (!visible) return null;

    return (
        <div className="flex items-center gap-2 px-3 py-1.5 bg-gray-800/95 border-b border-gray-700/50 backdrop-blur-sm">
            <Search size={13} className="text-gray-500 shrink-0" />
            <input
                ref={inputRef}
                type="text"
                value={query}
                onChange={handleQueryChange}
                onKeyDown={handleKeyDown}
                placeholder="Search messages..."
                className="flex-1 bg-transparent text-xs text-white placeholder-gray-500 focus:outline-none min-w-0"
                autoComplete="off"
            />
            {/* Role filter */}
            <div className="flex items-center gap-0.5 border-l border-gray-700 pl-2">
                {(['all', 'user', 'assistant'] as const).map(role => (
                    <button
                        key={role}
                        onClick={() => handleRoleFilterChange(role)}
                        className={`px-1.5 py-0.5 rounded text-[10px] transition-colors ${
                            roleFilter === role
                                ? 'bg-purple-600/30 text-purple-300'
                                : 'text-gray-500 hover:text-gray-300'
                        }`}
                    >
                        {role === 'all' ? 'All' : role === 'user' ? 'User' : 'AI'}
                    </button>
                ))}
            </div>
            {/* Navigation */}
            <div className="flex items-center gap-1 border-l border-gray-700 pl-2">
                <button
                    onClick={goPrev}
                    disabled={matches.length === 0}
                    className="p-0.5 text-gray-400 hover:text-white disabled:opacity-30 transition-colors"
                >
                    <ChevronUp size={13} />
                </button>
                <button
                    onClick={goNext}
                    disabled={matches.length === 0}
                    className="p-0.5 text-gray-400 hover:text-white disabled:opacity-30 transition-colors"
                >
                    <ChevronDown size={13} />
                </button>
                <span className="text-[10px] text-gray-500 min-w-[50px] text-center">
                    {matches.length > 0
                        ? `${activeMatchIndex + 1}/${matches.length}`
                        : query.length >= 2 ? 'No results' : ''
                    }
                </span>
            </div>
            {/* Close */}
            <button onClick={onClose} className="p-0.5 text-gray-500 hover:text-white transition-colors" title={t('common.close')}>
                <X size={13} />
            </button>
        </div>
    );
};

export default ChatSearchOverlay;
