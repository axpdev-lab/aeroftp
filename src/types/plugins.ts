// AeroAgent Plugin System Types

import type { AIToolParameter, DangerLevel, AITool } from './tools';

export interface PluginToolDef {
    name: string;
    description: string;
    parameters: AIToolParameter[];
    dangerLevel: DangerLevel;
    command: string;
}

export interface PluginManifest {
    id: string;
    name: string;
    version: string;
    author: string;
    tools: PluginToolDef[];
    enabled?: boolean;
}

/** Convert a PluginManifest's tools to AITool[] for merging with AGENT_TOOLS */
export function pluginToolsToAITools(manifest: PluginManifest): AITool[] {
    if (!manifest.enabled) return [];
    return manifest.tools.map(t => ({
        name: t.name,
        description: `[Plugin: ${manifest.name}] ${t.description}`,
        parameters: t.parameters,
        dangerLevel: t.dangerLevel,
    }));
}

/** Convert all plugin manifests to a flat AITool array */
export function allPluginTools(manifests: PluginManifest[]): AITool[] {
    return manifests.flatMap(pluginToolsToAITools);
}

/** Find which plugin owns a tool name */
export function findPluginForTool(
    manifests: PluginManifest[],
    toolName: string
): PluginManifest | undefined {
    return manifests.find(m => m.tools.some(t => t.name === toolName));
}
