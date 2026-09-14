// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Reuse ICU's expensive date-pattern setup across rows and renders. Keep just
 * one formatter per call site, so switching languages cannot grow a cache.
 * Renew on use after a minute (no background timer), including after a clock
 * rollback, to pick up OS timezone changes without restarting the application.
 */
export function createDateTimeFormatter(options: Intl.DateTimeFormatOptions) {
    const formatOptions = { ...options };
    let cached: { locale: string; createdAt: number; formatter: Intl.DateTimeFormat } | undefined;
    return (date: Date, locale: string): string => {
        const now = Date.now();
        if (!cached || cached.locale !== locale || now < cached.createdAt || now - cached.createdAt >= 60_000) {
            cached = { locale, createdAt: now, formatter: new Intl.DateTimeFormat(locale, formatOptions) };
        }
        return cached.formatter.format(date);
    };
}
