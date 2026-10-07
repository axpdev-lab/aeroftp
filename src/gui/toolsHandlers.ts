// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { GuiHandlers } from './controller';
import { bindSettingsScope } from './settingsScope';
import type { ToolPanel } from './toolsSchema';

export interface ToolsHandlerDeps {
    open(tool?: ToolPanel): void;
    close(): void;
}
export function createToolsHandlers(deps: ToolsHandlerDeps): Required<Pick<GuiHandlers, 'toolsOpen' | 'toolsRead' | 'toolsClose'>> {
    return {
        toolsOpen: async (tool, parent) => {
            const scope = await bindSettingsScope(parent);
            await scope.step(() => deps.open(tool));
        },
        toolsClose: async parent => {
            const scope = await bindSettingsScope(parent);
            await scope.step(deps.close);
        },
        toolsRead: async parent => {
            const scope = await bindSettingsScope(parent);
            await scope.step(() => {}); // live committed metadata only, no content hydration
        },
    };
}
