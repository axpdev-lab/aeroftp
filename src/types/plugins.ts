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

/** Convert a PluginManifest's tools to AITool[] for merging with AGENT_TOOLS.
 *  SECURITY: Plugin tools always require user approval — if a plugin declares
 *  dangerLevel 'safe', it is overridden to 'medium' to prevent untrusted
 *  plugin code from executing without user confirmation. */
export function pluginToolsToAITools(manifest: PluginManifest): AITool[] {
    if (!manifest.enabled) return [];
    return manifest.tools.map(t => ({
        name: t.name,
        description: `[Plugin: ${manifest.name}] ${t.description}`,
        parameters: t.parameters,
        // Plugin tools must never be 'safe' — force minimum 'medium' to require approval
        dangerLevel: t.dangerLevel === 'safe' ? 'medium' : t.dangerLevel,
    }));
}

/** Convert all plugin manifests to a flat AITool array */
export function allPluginTools(manifests: PluginManifest[]): AITool[] {
    return manifests.flatMap(pluginToolsToAITools);
}

/** Find which plugin owns a tool name (only enabled plugins) */
export function findPluginForTool(
    manifests: PluginManifest[],
    toolName: string
): PluginManifest | undefined {
    return manifests.find(m => m.enabled !== false && m.tools.some(t => t.name === toolName));
}
