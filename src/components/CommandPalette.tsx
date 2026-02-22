import React, { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import { useTranslation } from '../i18n';
import {
    Search, FolderPlus, Archive, FileText, Pencil, Settings, Bot,
    MessageSquare, History, Zap, FolderSync, Shield, Music,
    Code, Terminal, FolderOpen, Globe, Plus, Layers, X
} from 'lucide-react';

export type CommandCategory = 'file' | 'ai' | 'navigation' | 'tools' | 'sync';

export interface CommandItem {
    id: string;
    label: string;
    category: CommandCategory;
    icon: React.ReactNode;
    action: () => void;
    shortcut?: string;
    keywords?: string[];
}

interface CommandPaletteProps {
    commands: CommandItem[];
    onClose: () => void;
}

const CATEGORY_ORDER: CommandCategory[] = ['navigation', 'file', 'ai', 'tools', 'sync'];

const CATEGORY_ICONS: Record<CommandCategory, React.ReactNode> = {
    file: <FileText size={10} />,
    ai: <Bot size={10} />,
    navigation: <Globe size={10} />,
    tools: <Code size={10} />,
    sync: <FolderSync size={10} />,
};

export const CommandPalette: React.FC<CommandPaletteProps> = ({ commands, onClose }) => {
    const t = useTranslation();
    const [query, setQuery] = useState('');
    const [selectedIndex, setSelectedIndex] = useState(0);
    const inputRef = useRef<HTMLInputElement>(null);
    const listRef = useRef<HTMLDivElement>(null);

    // Focus input on mount
    useEffect(() => {
        setTimeout(() => inputRef.current?.focus(), 50);
    }, []);

    // Escape key
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('keydown', handler);
        return () => document.removeEventListener('keydown', handler);
    }, [onClose]);

    // Fuzzy filter
    const filtered = useMemo(() => {
        if (!query.trim()) return commands;
        const q = query.toLowerCase();
        return commands.filter(cmd =>
            cmd.label.toLowerCase().includes(q) ||
            cmd.category.includes(q) ||
            cmd.keywords?.some(k => k.toLowerCase().includes(q))
        );
    }, [commands, query]);

    // Group by category
    const grouped = useMemo(() => {
        const groups = new Map<CommandCategory, CommandItem[]>();
        for (const cmd of filtered) {
            const list = groups.get(cmd.category) || [];
            list.push(cmd);
            groups.set(cmd.category, list);
        }
        // Sort by predefined order
        const ordered: { category: CommandCategory; items: CommandItem[] }[] = [];
        for (const cat of CATEGORY_ORDER) {
            const items = groups.get(cat);
            if (items && items.length > 0) ordered.push({ category: cat, items });
        }
        return ordered;
    }, [filtered]);

    // Flat list for keyboard navigation
    const flatList = useMemo(() => filtered, [filtered]);

    // Reset selection when query changes
    useEffect(() => {
        setSelectedIndex(0);
    }, [query]);

    // Scroll selected item into view
    useEffect(() => {
        const el = listRef.current?.querySelector(`[data-index="${selectedIndex}"]`);
        el?.scrollIntoView({ block: 'nearest' });
    }, [selectedIndex]);

    const executeCommand = useCallback((cmd: CommandItem) => {
        onClose();
        // Defer action to avoid closing issues
        requestAnimationFrame(() => cmd.action());
    }, [onClose]);

    const handleKeyDown = (e: React.KeyboardEvent) => {
        if (e.key === 'ArrowDown') {
            e.preventDefault();
            setSelectedIndex(i => Math.min(i + 1, flatList.length - 1));
        } else if (e.key === 'ArrowUp') {
            e.preventDefault();
            setSelectedIndex(i => Math.max(i - 1, 0));
        } else if (e.key === 'Enter' && flatList[selectedIndex]) {
            e.preventDefault();
            executeCommand(flatList[selectedIndex]);
        }
    };

    let flatIndex = -1;

    return (
        <div
            className="fixed inset-0 z-[70] flex items-start justify-center pt-[15vh]"
            onClick={onClose}
        >
            <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" />
            <div
                className="relative w-full max-w-lg rounded-xl overflow-hidden
                    bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700
                    shadow-2xl flex flex-col max-h-[60vh]"
                onClick={e => e.stopPropagation()}
                onKeyDown={handleKeyDown}
            >
                {/* Search input */}
                <div className="flex items-center gap-2 px-4 py-3 border-b border-gray-200 dark:border-gray-700">
                    <Search size={16} className="text-gray-400 shrink-0" />
                    <input
                        ref={inputRef}
                        type="text"
                        value={query}
                        onChange={e => setQuery(e.target.value)}
                        placeholder={t('commandPalette.placeholder')}
                        className="flex-1 bg-transparent text-sm text-gray-900 dark:text-gray-100
                            placeholder:text-gray-400 dark:placeholder:text-gray-500
                            focus:outline-none"
                    />
                    <kbd className="hidden sm:inline-flex items-center px-1.5 py-0.5 text-[9px] font-mono
                        bg-gray-100 dark:bg-gray-800 text-gray-400 rounded border border-gray-200 dark:border-gray-700">
                        ESC
                    </kbd>
                </div>

                {/* Results */}
                <div ref={listRef} className="flex-1 overflow-y-auto py-2">
                    {filtered.length === 0 ? (
                        <div className="text-center py-8 text-gray-500 text-sm">
                            {t('commandPalette.noResults')}
                        </div>
                    ) : (
                        grouped.map(({ category, items }) => (
                            <div key={category}>
                                <div className="flex items-center gap-1.5 px-4 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500">
                                    {CATEGORY_ICONS[category]}
                                    {t(`commandPalette.category.${category}`)}
                                </div>
                                {items.map(cmd => {
                                    flatIndex++;
                                    const idx = flatIndex;
                                    return (
                                        <button
                                            key={cmd.id}
                                            data-index={idx}
                                            onClick={() => executeCommand(cmd)}
                                            className={`w-full flex items-center gap-3 px-4 py-2 text-sm transition-colors ${
                                                idx === selectedIndex
                                                    ? 'bg-purple-50 dark:bg-purple-900/30 text-purple-700 dark:text-purple-300'
                                                    : 'text-gray-700 dark:text-gray-300 hover:bg-gray-50 dark:hover:bg-gray-800'
                                            }`}
                                        >
                                            <span className="shrink-0 w-5 h-5 flex items-center justify-center text-gray-400">
                                                {cmd.icon}
                                            </span>
                                            <span className="flex-1 text-left truncate">{cmd.label}</span>
                                            {cmd.shortcut && (
                                                <kbd className="shrink-0 px-1.5 py-0.5 text-[9px] font-mono
                                                    bg-gray-100 dark:bg-gray-800 text-gray-400 rounded border border-gray-200 dark:border-gray-700">
                                                    {cmd.shortcut}
                                                </kbd>
                                            )}
                                        </button>
                                    );
                                })}
                            </div>
                        ))
                    )}
                </div>
            </div>
        </div>
    );
};

// Re-export icons for command definitions in App.tsx
export {
    FolderPlus, Archive, FileText, Pencil, Settings, Bot,
    MessageSquare, History, Zap, FolderSync, Shield, Music,
    Code, Terminal, FolderOpen, Globe, Plus, Layers
};
