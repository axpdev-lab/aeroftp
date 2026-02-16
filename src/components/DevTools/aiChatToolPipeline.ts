import { AgentToolCall } from '../../types/tools';

interface ExecutionLevel {
    tools: AgentToolCall[];
}

/**
 * Extract all paths from a tool call's arguments.
 */
function extractPaths(args: Record<string, unknown>): string[] {
    const paths: string[] = [];
    for (const key of ['path', 'local_path', 'remote_path', 'from', 'to']) {
        const val = args[key];
        if (typeof val === 'string' && val.trim()) {
            paths.push(val.trim());
        }
    }
    return paths;
}

/**
 * Determine if a tool mutates its target path (vs read-only).
 */
function isMutatingTool(toolName: string): boolean {
    const mutators = new Set([
        'local_edit', 'local_write', 'local_delete', 'local_rename', 'local_move_files', 'local_mkdir',
        'local_batch_rename', 'local_copy_files', 'local_trash',
        'remote_edit', 'remote_upload', 'remote_delete', 'remote_rename', 'remote_mkdir',
        'upload_files', 'download_files',
        'archive_compress', 'archive_decompress',
    ]);
    return mutators.has(toolName);
}

/**
 * Build execution levels using topological ordering based on path dependencies.
 * If tool A reads path X and tool B mutates path X, B depends on A (or vice versa).
 * Mutating tools on the same path are serialized.
 */
export function buildExecutionLevels(toolCalls: AgentToolCall[]): ExecutionLevel[] {
    if (toolCalls.length <= 1) {
        return [{ tools: toolCalls }];
    }

    const n = toolCalls.length;
    // dependency[i] = set of indices that tool i depends on
    const deps: Set<number>[] = Array.from({ length: n }, () => new Set<number>());

    // Build dependency graph
    for (let i = 0; i < n; i++) {
        const pathsI = extractPaths(toolCalls[i].args);
        const iMutates = isMutatingTool(toolCalls[i].toolName);

        for (let j = 0; j < i; j++) {
            const pathsJ = extractPaths(toolCalls[j].args);
            const jMutates = isMutatingTool(toolCalls[j].toolName);

            // Check for shared paths
            const shared = pathsI.some(p => pathsJ.includes(p));
            if (shared && (iMutates || jMutates)) {
                // i depends on j (j comes first since j < i)
                deps[i].add(j);
            }
        }
    }

    // Topological sort into levels (Kahn's algorithm)
    const levels: ExecutionLevel[] = [];
    const completed = new Set<number>();

    while (completed.size < n) {
        const level: number[] = [];
        for (let i = 0; i < n; i++) {
            if (completed.has(i)) continue;
            // Check if all dependencies are completed
            let ready = true;
            for (const dep of deps[i]) {
                if (!completed.has(dep)) {
                    ready = false;
                    break;
                }
            }
            if (ready) level.push(i);
        }

        if (level.length === 0) {
            // Circular dependency fallback — execute remaining sequentially
            for (let i = 0; i < n; i++) {
                if (!completed.has(i)) {
                    levels.push({ tools: [toolCalls[i]] });
                    completed.add(i);
                }
            }
            break;
        }

        levels.push({ tools: level.map(i => toolCalls[i]) });
        for (const i of level) completed.add(i);
    }

    return levels;
}

/**
 * Execute tool calls in pipeline order: parallel within each level, sequential between levels.
 */
export async function executePipeline(
    levels: ExecutionLevel[],
    executor: (tc: AgentToolCall) => Promise<string | null>,
): Promise<string[]> {
    const allResults: string[] = [];

    for (const level of levels) {
        const settled = await Promise.allSettled(level.tools.map(tc => executor(tc)));
        for (const result of settled) {
            if (result.status === 'fulfilled' && result.value !== null) {
                allResults.push(result.value);
            }
        }
    }

    return allResults;
}
