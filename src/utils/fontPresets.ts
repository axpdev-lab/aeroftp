// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Known font-family presets offered by Settings > Appearance. The GUI
// controller allowlist accepts only these values; a custom free-text family
// stays a human-only edit.

import { DEFAULT_APP_FONT_FAMILY } from '../hooks/useSettings';
import { translate } from '../i18n';

// Font names are product names and stay as written; the two worded labels are
// read through the global translator at access time, because Settings indexes
// this table directly.
export const FONT_PRESETS: { readonly label: string; value: string; bundled: boolean }[] = [
    {
        get label(): string { return translate('settings.fontPresetDefault', { font: 'Inter' }); },
        value: DEFAULT_APP_FONT_FAMILY,
        bundled: true,
    },
    {
        get label(): string { return translate('settings.fontPresetSystemDefault'); },
        value: 'system-ui, -apple-system, sans-serif',
        bundled: true,
    },
    { label: 'FiraGO', value: "'FiraGO', sans-serif", bundled: false },
    { label: 'Noto Sans', value: "'Noto Sans', sans-serif", bundled: false },
    {
        label: 'JetBrains Mono',
        value: "'JetBrains Mono', monospace",
        bundled: false,
    },
];

export const KNOWN_FONT_FAMILIES: readonly string[] = FONT_PRESETS.map(preset => preset.value);
