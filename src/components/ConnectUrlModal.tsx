// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Link2, Loader2, X } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useGuardedClose } from '../hooks/useGuardedClose';
import {
    applyTemplate,
    filterSuggestions,
    nextPlaceholder,
    placeholderRanges,
    suggestionForValue,
    type UrlSchemeSuggestion,
} from '../utils/connectUrlSchemes';

interface ConnectUrlModalProps {
    onClose: () => void;
}

/**
 * File > Connect to URL...: one URL field, live scheme suggestions from the
 * launch-url table, Tab inserts a template and jumps between its
 * placeholders, Enter connects, Esc closes. The URL is handed to the
 * `open_launch_url` command, which parses it with the same `launch_url::parse`
 * as a second-instance launch and queues it for the existing tab-reuse
 * dispatch. The raw URL may hold credentials: it is never logged and never
 * echoed back, only the backend's parse error is shown.
 */
export function ConnectUrlModal({ onClose }: ConnectUrlModalProps) {
    const t = useTranslation();
    const [value, setValue] = useState('');
    const [error, setError] = useState<string | null>(null);
    const [submitting, setSubmitting] = useState(false);
    const [active, setActive] = useState(0);
    const [listVisible, setListVisible] = useState(true);
    const inputRef = useRef<HTMLInputElement>(null);
    const { requestClose, onBackdropClick } = useGuardedClose({ guard: null, onClose, escape: true });

    const suggestions = filterSuggestions(value);
    const signIn = suggestionForValue(value)?.signIn === true;

    const applySuggestion = (suggestion: UrlSchemeSuggestion) => {
        const applied = applyTemplate(suggestion.template);
        setValue(applied.value);
        setActive(0);
        // The selection applies to the committed value, after React flushes it.
        requestAnimationFrame(() => {
            const input = inputRef.current;
            if (input && applied.selection) input.setSelectionRange(applied.selection.start, applied.selection.end);
        });
    };

    const submit = async () => {
        const url = value.trim();
        if (!url || submitting) return;
        setSubmitting(true);
        setError(null);
        try {
            await invoke('open_launch_url', { url });
            onClose();
        } catch (err) {
            setError(typeof err === 'string' ? err : String(err));
        } finally {
            setSubmitting(false);
        }
    };

    const onKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
        const input = inputRef.current;
        if (!input) return;
        if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
            if (listVisible && suggestions.length > 0) {
                e.preventDefault();
                const delta = e.key === 'ArrowDown' ? 1 : -1;
                setActive((prev) => (prev + delta + suggestions.length) % suggestions.length);
            }
        } else if (e.key === 'Tab') {
            e.preventDefault();
            if (placeholderRanges(value).length > 0) {
                const next = nextPlaceholder(value, input.selectionStart ?? value.length);
                const at = next ?? { start: value.length, end: value.length, name: '' };
                input.setSelectionRange(at.start, at.end);
            } else if (suggestions.length > 0) {
                applySuggestion(suggestions[Math.min(active, suggestions.length - 1)]);
            }
        } else if (e.key === 'Enter') {
            e.preventDefault();
            void submit();
        }
    };

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[18vh] bg-black/50 backdrop-blur-sm animate-fade-in"
            role="dialog"
            aria-modal="true"
            aria-labelledby="connect-url-title"
            onClick={onBackdropClick}
        >
            <div
                className="w-full max-w-lg bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in"
                onClick={(e) => e.stopPropagation()}
            >
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3 min-w-0">
                        <div className="p-2 bg-blue-100 dark:bg-blue-900/40 rounded-lg shrink-0">
                            <Link2 size={18} className="text-blue-600 dark:text-blue-400" />
                        </div>
                        <h2 id="connect-url-title" className="text-base font-semibold truncate">
                            {t('connectUrl.title')}
                        </h2>
                    </div>
                    <button
                        onClick={requestClose}
                        className="p-1.5 rounded-lg text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                        aria-label={t('common.close')}
                    >
                        <X size={16} />
                    </button>
                </div>
                <div className="px-5 py-4">
                    <div className="relative">
                        <input
                            ref={inputRef}
                            type="text"
                            autoFocus
                            value={value}
                            spellCheck={false}
                            autoComplete="off"
                            placeholder={t('connectUrl.placeholder')}
                            onChange={(e) => { setValue(e.target.value); setError(null); setActive(0); setListVisible(true); }}
                            onKeyDown={onKeyDown}
                            onFocus={() => setListVisible(true)}
                            onBlur={() => setListVisible(false)}
                            className="w-full px-3 py-2 border rounded-lg text-sm font-mono bg-gray-50 dark:bg-gray-700 border-gray-300 dark:border-gray-600 focus:outline-none focus:ring-2 focus:ring-blue-500"
                        />
                        {listVisible && suggestions.length > 0 && (
                            <ul className="absolute left-0 right-0 top-full mt-1 z-10 max-h-48 overflow-auto rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg">
                                {suggestions.map((suggestion, i) => (
                                    <li key={suggestion.scheme}>
                                        <button
                                            type="button"
                                            // mousedown keeps the input focused, so the template's
                                            // first placeholder can be selected right away.
                                            onMouseDown={(e) => { e.preventDefault(); applySuggestion(suggestion); }}
                                            onMouseEnter={() => setActive(i)}
                                            className={`w-full flex items-center gap-2 px-3 py-1.5 text-left text-sm ${i === active ? 'bg-blue-100 dark:bg-blue-900/40' : 'hover:bg-gray-100 dark:hover:bg-gray-700'}`}
                                        >
                                            <span className="font-mono text-blue-600 dark:text-blue-400 shrink-0">{suggestion.scheme}://</span>
                                            <span className="truncate text-gray-600 dark:text-gray-300">{suggestion.label}</span>
                                        </button>
                                    </li>
                                ))}
                            </ul>
                        )}
                    </div>
                    {error && (
                        <p className="mt-2 text-xs text-red-600 dark:text-red-400 break-words">{error}</p>
                    )}
                    {signIn && !error && (
                        <p className="mt-2 text-xs text-gray-500 dark:text-gray-400">{t('connectUrl.signInHint')}</p>
                    )}
                </div>
                <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        onClick={requestClose}
                        className="px-4 py-2 text-sm rounded-lg border border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        onClick={() => void submit()}
                        disabled={submitting || !value.trim()}
                        className="px-4 py-2 text-sm rounded-lg bg-blue-600 text-white hover:bg-blue-700 transition-colors disabled:opacity-50 flex items-center gap-2"
                    >
                        {submitting && <Loader2 size={14} className="animate-spin" />}
                        {t('common.connect')}
                    </button>
                </div>
            </div>
        </div>
    );
}
