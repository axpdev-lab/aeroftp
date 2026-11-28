// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderField } from './types';

/** Shown when a field has a pattern but no message of its own. */
export const DEFAULT_PATTERN_MESSAGE_KEY = 'connection.fieldPatternInvalid';

/**
 * The i18n key of the live warning for this value, or null when the value is
 * fine. An empty value is never flagged here (required-ness is separate), and a
 * pattern that does not compile flags nothing (the registry test catches it).
 */
export function fieldPatternIssue(field: ProviderField | undefined, value: string | undefined): string | null {
    if (!field?.pattern || !value) return null;
    let re: RegExp;
    try {
        re = new RegExp(field.pattern);
    } catch {
        return null;
    }
    return re.test(value) ? null : field.patternMessageKey ?? DEFAULT_PATTERN_MESSAGE_KEY;
}
