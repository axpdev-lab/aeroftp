import { invoke } from '@tauri-apps/api/core';
import { ProjectContext } from '../../types/contextIntelligence';

// Cache to avoid repeated detection for same path
let cachedPath: string | null = null;
let cachedContext: ProjectContext | null = null;

/**
 * Detect project type and metadata. Results are cached per path.
 */
export async function detectProjectContext(path: string): Promise<ProjectContext | null> {
    if (!path) return null;

    // Return cache if path hasn't changed
    if (cachedPath === path && cachedContext) return cachedContext;

    try {
        const result = await invoke<ProjectContext>('detect_project_context', { path });
        cachedPath = path;
        cachedContext = result;
        return result;
    } catch {
        cachedPath = path;
        cachedContext = null;
        return null;
    }
}

/**
 * Invalidate cache (call when path changes significantly)
 */
export function invalidateProjectCache(): void {
    cachedPath = null;
    cachedContext = null;
}

/**
 * Fetch file imports for a given file path
 */
export async function fetchFileImports(filePath: string): Promise<string[]> {
    if (!filePath) return [];
    try {
        const imports = await invoke<Array<{ source: string; resolved_path: string | null }>>('scan_file_imports', { path: filePath });
        return imports
            .filter(imp => imp.resolved_path)
            .map(imp => imp.resolved_path!)
            .slice(0, 15);
    } catch {
        return [];
    }
}

/**
 * Fetch git context for a directory
 */
export async function fetchGitContext(path: string): Promise<{ branch: string; summary: string } | null> {
    if (!path) return null;
    try {
        const git = await invoke<{
            branch: string;
            recent_commits: Array<{ hash: string; message: string }>;
            uncommitted_changes: string[];
            has_uncommitted: boolean;
        }>('get_git_context', { path });

        const lines: string[] = [];
        lines.push(`- Branch: ${git.branch}`);
        if (git.has_uncommitted) {
            lines.push(`- Uncommitted: ${git.uncommitted_changes.length} file(s) changed`);
            // Show first few changes
            git.uncommitted_changes.slice(0, 5).forEach(c => lines.push(`  ${c}`));
        }
        if (git.recent_commits.length > 0) {
            lines.push(`- Recent commits:`);
            git.recent_commits.slice(0, 5).forEach(c =>
                lines.push(`  ${c.hash} ${c.message}`)
            );
        }

        return { branch: git.branch, summary: lines.join('\n') };
    } catch {
        return null;
    }
}
