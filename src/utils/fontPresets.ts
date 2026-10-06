// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Known font-family presets offered by Settings > Appearance. The GUI
// controller allowlist accepts only these values; a custom free-text family
// stays a human-only edit.

import { DEFAULT_APP_FONT_FAMILY } from '../hooks/useSettings';

export const FONT_PRESETS = [
    { label: 'Inter (Default)', value: DEFAULT_APP_FONT_FAMILY, bundled: true },
    {
        label: 'System Default',
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
