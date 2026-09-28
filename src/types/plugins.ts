// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// AeroAgent Plugin System Types

import type { AIToolParameter, DangerLevel } from './tools';

export interface PluginToolDef {
    name: string;
    description: string;
    parameters: AIToolParameter[];
    dangerLevel: DangerLevel;
    command: string;
    integrity?: string;
}

export interface PluginManifest {
    id: string;
    name: string;
    version: string;
    author: string;
    tools: PluginToolDef[];
    enabled?: boolean;
}
