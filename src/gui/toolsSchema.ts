// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export const TOOLS_INTENTS = ['tools_open', 'tools_read', 'tools_close'] as const;
export const TOOL_PANELS = ['editor', 'terminal', 'agent'] as const;
export type ToolPanel = typeof TOOL_PANELS[number];
export interface GuiToolsProjection {
    open: boolean;
    visible_panels: (ToolPanel | 'security')[];
    /** Security Tools owns staged secrets: agent workspace changes are blocked. */
    protected: boolean;
}
/** Only fixed panel names and booleans; no editor text, terminal output or chat. */
export function buildToolsProjection(source: GuiToolsProjection): GuiToolsProjection {
    return { open: source.open === true, protected: source.protected === true,
        visible_panels: source.open === true ? [...new Set(source.visible_panels)]
            .filter(panel => [...TOOL_PANELS, 'security'].includes(panel)).slice(0, 4) : [] };
}
