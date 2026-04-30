// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef, useState } from 'react';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';
import { clampAppFontSize, MIN_APP_FONT_SIZE, MAX_APP_FONT_SIZE } from './useSettings';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

interface Indicator {
    size: number;
    seq: number;
}

export const useFontSizeShortcuts = (
    fontSize: number,
    setFontSize: (n: number) => void
): Indicator | null => {
    const [indicator, setIndicator] = useState<Indicator | null>(null);
    const fontSizeRef = useRef(fontSize);
    fontSizeRef.current = fontSize;
    const seqRef = useRef(0);

    useEffect(() => {
        const persist = async (newSize: number) => {
            try {
                const existing = await secureGetWithFallback<Record<string, unknown>>(
                    SETTINGS_VAULT_KEY,
                    SETTINGS_KEY,
                );
                const updated = { ...(existing || {}), fontSize: newSize };
                await secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, updated);
                window.dispatchEvent(
                    new CustomEvent('aeroftp-settings-changed', { detail: updated }),
                );
            } catch {
                /* ignore */
            }
        };

        const change = (delta: number) => {
            const next = clampAppFontSize(fontSizeRef.current + delta);
            if (next === fontSizeRef.current) return;
            setFontSize(next);
            void persist(next);
            seqRef.current += 1;
            setIndicator({ size: next, seq: seqRef.current });
        };

        const setExact = (size: number) => {
            const next = clampAppFontSize(size);
            if (next === fontSizeRef.current) return;
            setFontSize(next);
            void persist(next);
            seqRef.current += 1;
            setIndicator({ size: next, seq: seqRef.current });
        };

        const isEditableTarget = (t: EventTarget | null): boolean => {
            const el = t as HTMLElement | null;
            if (!el) return false;
            const tag = el.tagName;
            if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true;
            if (el.isContentEditable) return true;
            return false;
        };

        const onKeyDown = (e: KeyboardEvent) => {
            if (!(e.ctrlKey || e.metaKey)) return;
            if (e.altKey) return;
            // Skip when typing in inputs so native field shortcuts keep working
            if (isEditableTarget(e.target)) return;
            if (e.key === '+' || e.key === '=' || e.code === 'NumpadAdd') {
                e.preventDefault();
                change(+1);
            } else if (e.key === '-' || e.key === '_' || e.code === 'NumpadSubtract') {
                e.preventDefault();
                change(-1);
            } else if (e.key === '0' || e.code === 'Numpad0') {
                e.preventDefault();
                setExact(16);
            }
        };

        const onWheel = (e: WheelEvent) => {
            if (!(e.ctrlKey || e.metaKey)) return;
            // Don't hijack zoom in Monaco editor, terminal, or anything opted out
            const target = e.target as HTMLElement | null;
            if (
                target?.closest(
                    '.monaco-editor, .xterm, [data-no-font-zoom], textarea, input',
                )
            ) {
                return;
            }
            e.preventDefault();
            change(e.deltaY < 0 ? +1 : -1);
        };

        window.addEventListener('keydown', onKeyDown);
        window.addEventListener('wheel', onWheel, { passive: false });
        return () => {
            window.removeEventListener('keydown', onKeyDown);
            window.removeEventListener('wheel', onWheel);
        };
    }, [setFontSize]);

    // Auto-hide the indicator
    useEffect(() => {
        if (!indicator) return;
        const t = window.setTimeout(() => setIndicator(null), 1200);
        return () => window.clearTimeout(t);
    }, [indicator]);

    return indicator;
};

interface FontSizeIndicatorProps {
    indicator: Indicator | null;
}

export const FontSizeIndicator: React.FC<FontSizeIndicatorProps> = ({ indicator }) => {
    if (!indicator) return null;
    const atMin = indicator.size <= MIN_APP_FONT_SIZE;
    const atMax = indicator.size >= MAX_APP_FONT_SIZE;
    return (
        <div
            key={indicator.seq}
            role="status"
            aria-live="polite"
            className="pointer-events-none fixed bottom-6 left-1/2 -translate-x-1/2 z-[10000] animate-fade-in"
            style={{ animation: 'fadeInUp 180ms ease-out' }}
        >
            <div className="flex items-center gap-2 px-3 py-1.5 rounded-full text-xs font-medium bg-gray-900/90 dark:bg-gray-100/90 text-white dark:text-gray-900 backdrop-blur shadow-lg">
                <span className="opacity-60">Font</span>
                <span className="tabular-nums">{indicator.size}px</span>
                {(atMin || atMax) && (
                    <span className="opacity-60 text-[10px] uppercase tracking-wider">
                        {atMin ? 'min' : 'max'}
                    </span>
                )}
            </div>
        </div>
    );
};
