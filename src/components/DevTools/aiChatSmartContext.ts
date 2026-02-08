import { TaskType } from '../../types/ai';
import { ProjectContext, ContextSection, SmartContext } from '../../types/contextIntelligence';
import { estimateTokens } from './aiChatUtils';

// Keywords that boost priority for specific context types
const GIT_KEYWORDS = /\b(git|commit|push|pull|merge|branch|diff|change|changed|history|log|revert|stash)\b/i;
const BUG_KEYWORDS = /\b(bug|fix|error|crash|issue|problem|broken|fail|exception|debug|trace)\b/i;
const DEPS_KEYWORDS = /\b(install|dependency|dependencies|package|npm|cargo|pip|require|import|module|library|crate|version)\b/i;
const FILE_KEYWORDS = /\b(file|read|write|edit|create|delete|rename|move|path)\b/i;
const PROJECT_KEYWORDS = /\b(project|config|setup|init|scaffold|structure|architecture)\b/i;

/**
 * Analyze user prompt to determine which context types are most relevant.
 * Returns priority map: lower number = higher priority.
 */
function analyzePromptIntent(prompt: string, taskType: TaskType): Record<string, number> {
    const priorities: Record<string, number> = {
        project: 5,
        git: 5,
        imports: 5,
        memory: 3,  // Memory is almost always useful
        rag: 4,
    };

    // Boost based on keyword matching (use Math.min to preserve highest priority = lowest number)
    if (GIT_KEYWORDS.test(prompt)) priorities.git = Math.min(priorities.git, 1);
    if (BUG_KEYWORDS.test(prompt)) { priorities.memory = Math.min(priorities.memory, 1); priorities.imports = Math.min(priorities.imports, 2); }
    if (DEPS_KEYWORDS.test(prompt)) priorities.project = Math.min(priorities.project, 1);
    if (FILE_KEYWORDS.test(prompt)) { priorities.imports = Math.min(priorities.imports, 2); priorities.rag = Math.min(priorities.rag, 2); }
    if (PROJECT_KEYWORDS.test(prompt)) priorities.project = Math.min(priorities.project, 1);

    // Boost based on task type (use Math.min to not overwrite higher keyword priorities)
    switch (taskType) {
        case 'code_generation': priorities.imports = Math.min(priorities.imports, 2); priorities.project = Math.min(priorities.project, 2); break;
        case 'code_review': priorities.git = Math.min(priorities.git, 2); priorities.imports = Math.min(priorities.imports, 1); break;
        case 'file_analysis': priorities.rag = Math.min(priorities.rag, 1); priorities.imports = Math.min(priorities.imports, 2); break;
        case 'terminal_command': priorities.project = Math.min(priorities.project, 2); break;
    }

    return priorities;
}

/**
 * Build smart context sections based on available data and user prompt.
 */
export function buildSmartContext(
    userPrompt: string,
    taskType: TaskType,
    projectContext: ProjectContext | null,
    gitSummary: string | null,
    agentMemory: string,
    editorImports: string[],
    ragSummary: string | null,
    tokenBudget: number,
): SmartContext {
    const priorities = analyzePromptIntent(userPrompt, taskType);
    const sections: ContextSection[] = [];

    // Build sections with their priorities
    if (projectContext) {
        const content = formatProjectSection(projectContext);
        sections.push({
            type: 'project',
            content,
            priority: priorities.project,
            estimatedTokens: estimateTokens(content),
        });
    }

    if (gitSummary) {
        sections.push({
            type: 'git',
            content: gitSummary,
            priority: priorities.git,
            estimatedTokens: estimateTokens(gitSummary),
        });
    }

    if (agentMemory.trim()) {
        // Trim memory to recent entries if too long
        const memoryLines = agentMemory.trim().split('\n');
        const recentMemory = memoryLines.slice(-20).join('\n');
        sections.push({
            type: 'memory',
            content: recentMemory,
            priority: priorities.memory,
            estimatedTokens: estimateTokens(recentMemory),
        });
    }

    if (editorImports.length > 0) {
        const content = `Imported files: ${editorImports.map(p => {
            const parts = p.replace(/\\/g, '/').split('/');
            return parts[parts.length - 1];
        }).join(', ')}`;
        sections.push({
            type: 'imports',
            content,
            priority: priorities.imports,
            estimatedTokens: estimateTokens(content),
        });
    }

    if (ragSummary) {
        sections.push({
            type: 'rag',
            content: ragSummary,
            priority: priorities.rag,
            estimatedTokens: estimateTokens(ragSummary),
        });
    }

    // Sort by priority (ascending = highest priority first)
    sections.sort((a, b) => a.priority - b.priority);

    // Trim to fit token budget
    const fittedSections: ContextSection[] = [];
    let totalTokens = 0;

    for (const section of sections) {
        if (totalTokens + section.estimatedTokens <= tokenBudget) {
            fittedSections.push(section);
            totalTokens += section.estimatedTokens;
        } else if (section.priority <= 2) {
            // High-priority sections get compressed instead of dropped
            const availableTokens = tokenBudget - totalTokens;
            if (availableTokens > 50) {
                const truncatedContent = section.content.slice(0, (availableTokens - 1) * 4);
                const truncatedTokens = estimateTokens(truncatedContent);
                fittedSections.push({
                    ...section,
                    content: truncatedContent + '...',
                    estimatedTokens: truncatedTokens,
                });
                totalTokens += truncatedTokens;
            }
        }
        // Low-priority sections are simply dropped when budget is tight
    }

    return {
        sections: fittedSections,
        totalEstimatedTokens: totalTokens,
    };
}

/**
 * Format smart context into a string for system prompt injection
 */
export function formatSmartContextForPrompt(ctx: SmartContext): string {
    if (ctx.sections.length === 0) return '';

    const parts: string[] = [];

    for (const section of ctx.sections) {
        switch (section.type) {
            case 'project':
                parts.push(section.content);
                break;
            case 'git':
                parts.push(section.content);
                break;
            case 'memory':
                parts.push(`- Agent memory:\n${section.content}`);
                break;
            case 'imports':
                parts.push(`- ${section.content}`);
                break;
            case 'rag':
                parts.push(section.content);
                break;
        }
    }

    return parts.join('\n');
}

function formatProjectSection(ctx: ProjectContext): string {
    const lines: string[] = [];
    const nameVersion = [ctx.name, ctx.version ? `v${ctx.version}` : null]
        .filter(Boolean).join(' ');
    lines.push(`- Project: ${nameVersion || 'unnamed'} (${ctx.project_type})`);
    if (ctx.scripts.length > 0) {
        lines.push(`- Scripts: ${ctx.scripts.slice(0, 8).join(', ')}`);
    }
    if (ctx.deps_count > 0 || ctx.dev_deps_count > 0) {
        const parts: string[] = [];
        if (ctx.deps_count > 0) parts.push(`${ctx.deps_count} production`);
        if (ctx.dev_deps_count > 0) parts.push(`${ctx.dev_deps_count} dev`);
        lines.push(`- Dependencies: ${parts.join(', ')}`);
    }
    if (ctx.entry_points.length > 0) {
        lines.push(`- Entry: ${ctx.entry_points.join(', ')}`);
    }
    return lines.join('\n');
}

/**
 * Determine budget mode based on available token budget
 */
export function determineBudgetMode(modelMaxTokens: number): 'full' | 'compact' | 'minimal' {
    if (modelMaxTokens >= 32000) return 'full';
    if (modelMaxTokens >= 8000) return 'compact';
    return 'minimal';
}
