// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { PanelVisibility } from '../components/DevTools/types';
import { GuiError } from './errors';
import type { ToolPanel } from './toolsSchema';

export interface ToolsControl { ensure(tool?: ToolPanel): void; close(): void; }
export type RegisterToolsControl = (control: ToolsControl) => () => void;
export const toolsPanelCapacity = (width: number) => width < 600 ? 1 : width < 900 ? 2 : width < 1000 ? 3 : 4;
/** Never hide/unmount another panel or discard its editor/PTY state to make room. */
export function panelsWithAgentTool(panels: PanelVisibility, tool: ToolPanel | undefined, width: number): PanelVisibility {
    if (panels.security) throw new GuiError('blocked');
    if (!tool) return panels;
    const key = tool === 'agent' ? 'chat' : tool;
    const enabled = (['editor', 'terminal', 'chat', 'security'] as const).filter(panel => panels[panel]);
    const capacity = toolsPanelCapacity(width);
    if (enabled.slice(0, capacity).includes(key)) return panels;
    if (enabled.length >= capacity) throw new GuiError('blocked');
    return { ...panels, [key]: true };
}
