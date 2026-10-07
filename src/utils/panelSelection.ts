// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type PanelSelectionMode = 'names' | 'all' | 'none';
export interface SelectionPanel {
    files: readonly { name: string }[];
    setSelection(selection: Set<string>): void;
}

/** Shared keyboard/menu/controller action. Never partially select a stale list. */
export function applyPanelSelection(panel: SelectionPanel, names: readonly string[], mode: PanelSelectionMode): boolean {
    if (!['names', 'all', 'none'].includes(mode)) return false;
    const available = new Set(panel.files.map(file => file.name));
    if (mode === 'names' && names.some(name => !available.has(name))) return false;
    panel.setSelection(new Set(mode === 'all' ? available : mode === 'none' ? [] : names));
    return true;
}
