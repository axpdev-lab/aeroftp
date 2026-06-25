// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { TaskType } from '../../types/ai';
import { BudgetMode, ProjectContext, SmartContext } from '../../types/contextIntelligence';
import { fetchCodingRules } from './aiChatRules';
import { formatMentionAttachmentsForPrompt, resolveContextMentions } from './aiChatMentions';
import { buildSmartContext } from './aiChatSmartContext';
import type { AgentMode } from './aiChatTypes';
import type { AgentPromptProfile, SystemPromptContext } from './aiChatSystemPrompt';

export interface CodingWorkspaceContext {
    projectPath: string;
    localPath?: string;
    remotePath?: string;
    projectContext: ProjectContext | null;
    gitBranch?: string;
    gitSummary?: string;
    fileImports: string[];
    ragSummary: string | null;
    codingRulesBlock: string;
    mentionContextBlock: string;
}

export interface BuildCodingWorkspaceContextArgs {
    projectPath?: string | null;
    localPath?: string;
    remotePath?: string;
    projectContext: ProjectContext | null;
    gitBranch?: string | null;
    gitSummary?: string | null;
    fileImports?: string[] | null;
    ragIndex?: Record<string, unknown> | null;
    userPrompt: string;
}

export interface BuildWorkspaceSmartContextArgs {
    userPrompt: string;
    taskType: TaskType;
    agentMemory: string;
    tokenBudget: number;
    budgetMode?: BudgetMode;
}

export interface BuildCodingPlanPromptBlockArgs {
    userPrompt: string;
    taskType: TaskType;
    agentMode?: AgentMode;
}

export interface ResolveAgentPromptProfileArgs {
    userPrompt: string;
    taskType: TaskType;
}

const CODING_PLAN_ACTION_RE = /\b(implement|fix|debug|repair|refactor|change|update|modify|add|remove|delete|rename|move|create|build|wire|integrate|migrate|convert|optimi[sz]e|test|typecheck|lint)\b/i;
const CODING_PLAN_CODE_RE = /\b(code|component|function|class|module|hook|api|bug|feature|test|typecheck|lint|typescript|javascript|rust|python|frontend|backend|workspace|repo|project|branch|file|files)\b/i;
const CODING_PLAN_EXPLICIT_RE = /\b(plan|architect|approach|design)\b/i;
const CODING_PLAN_SKIP_RE = /\b(no plan|do not plan|don't plan|skip plan|just answer|answer only)\b/i;
const CODING_PROFILE_CODE_RE = /\b(source|code|component|function|class|module|hook|api|bug|feature|test|tests|typecheck|lint|typescript|javascript|tsx|jsx|rust|cargo|python|pytest|frontend|backend|repo|repository|branch|diff|review|refactor|package\.json|cargo\.toml|tsconfig|vite|react)\b/i;
const CODING_PROFILE_ACTION_RE = /\b(implement|fix|debug|repair|refactor|change|update|modify|add|remove|delete|rename|move|create|build|wire|integrate|migrate|convert|optimi[sz]e|test|typecheck|lint|review)\b/i;
const CODING_PROFILE_TERMINAL_RE = /\b(npm|pnpm|yarn|cargo|pytest|go test|composer|phpunit|vitest|jest|eslint|tsc|typecheck|lint|build|test|dev server)\b/i;
const CODING_PROFILE_EXPLICIT_RE = /\b(coding agent|code agent|codex|work on this repo|work in this repo|software project)\b/i;
const FILE_MANAGER_PROFILE_RE = /\b(ftp|ftps|sftp|webdav|s3|server|remote|upload|download|sync|aerosync|aerocloud|connection|connect|host|port|bucket|oauth|credential|password|protocol|quota|list files|show files|browse files)\b/i;

function hasWorkspacePath(workspace: CodingWorkspaceContext): boolean {
    return !!(workspace.projectPath || workspace.localPath || workspace.remotePath);
}

function hasStrongCodingWorkspaceFacts(workspace: CodingWorkspaceContext): boolean {
    return !!(
        workspace.projectContext ||
        workspace.gitBranch ||
        workspace.gitSummary ||
        workspace.fileImports.length > 0 ||
        !!workspace.ragSummary ||
        workspace.codingRulesBlock.trim() ||
        workspace.mentionContextBlock.trim()
    );
}

function isFileManagerOnlyPrompt(prompt: string): boolean {
    return FILE_MANAGER_PROFILE_RE.test(prompt)
        && !CODING_PROFILE_CODE_RE.test(prompt)
        && !CODING_PROFILE_TERMINAL_RE.test(prompt)
        && !CODING_PROFILE_EXPLICIT_RE.test(prompt);
}

export function resolveAgentPromptProfile(
    workspace: CodingWorkspaceContext,
    args: ResolveAgentPromptProfileArgs,
): AgentPromptProfile {
    const prompt = args.userPrompt.trim();
    if (!prompt) return 'file_manager';

    const explicitCodingProfile = CODING_PROFILE_EXPLICIT_RE.test(prompt);
    const hasStrongWorkspaceFacts = hasStrongCodingWorkspaceFacts(workspace);
    const hasPathForExplicitCoding = hasWorkspacePath(workspace) && (explicitCodingProfile || CODING_PROFILE_CODE_RE.test(prompt));
    if (!hasStrongWorkspaceFacts && !hasPathForExplicitCoding) return 'file_manager';

    if (isFileManagerOnlyPrompt(prompt)) return 'file_manager';

    if (explicitCodingProfile) return 'coding_agent';

    if (args.taskType === 'code_generation' || args.taskType === 'code_review') {
        return 'coding_agent';
    }

    if (args.taskType === 'terminal_command' && CODING_PROFILE_TERMINAL_RE.test(prompt)) {
        return 'coding_agent';
    }

    if (args.taskType === 'file_analysis' && CODING_PROFILE_CODE_RE.test(prompt)) {
        return 'coding_agent';
    }

    if (CODING_PROFILE_ACTION_RE.test(prompt) && CODING_PROFILE_CODE_RE.test(prompt)) {
        return 'coding_agent';
    }

    return 'file_manager';
}

export function shouldRequestCodingPlanArtifact(
    workspace: CodingWorkspaceContext,
    args: BuildCodingPlanPromptBlockArgs,
): boolean {
    const prompt = args.userPrompt.trim();
    if (!prompt || CODING_PLAN_SKIP_RE.test(prompt)) return false;

    const hasWorkspaceFacts = !!(
        workspace.projectPath ||
        workspace.localPath ||
        workspace.projectContext ||
        workspace.gitBranch ||
        workspace.fileImports.length > 0 ||
        workspace.mentionContextBlock.trim()
    );
    if (!hasWorkspaceFacts) return false;

    if (CODING_PLAN_EXPLICIT_RE.test(prompt) && CODING_PLAN_CODE_RE.test(prompt)) return true;

    if (args.taskType === 'quick_answer' || args.taskType === 'file_analysis') {
        return false;
    }

    const looksCodingRelated = CODING_PLAN_CODE_RE.test(prompt)
        || args.taskType === 'code_generation'
        || args.taskType === 'code_review';
    if (!looksCodingRelated || !CODING_PLAN_ACTION_RE.test(prompt)) return false;

    const mentionsMultipleFiles = /\b(multi[-\s]?file|files|workspace|repo|project|app|feature|refactor|integration)\b/i.test(prompt);
    const isLongEnoughToBeNonTrivial = prompt.length >= 80;

    return mentionsMultipleFiles
        || isLongEnoughToBeNonTrivial
        || args.taskType === 'code_generation'
        || args.taskType === 'code_review';
}

export function formatWorkspaceRagSummary(ragIndex?: Record<string, unknown> | null): string | null {
    if (!ragIndex) return null;

    const rawExtensions = ragIndex.extensions;
    const extensions = rawExtensions && typeof rawExtensions === 'object'
        ? rawExtensions as Record<string, number>
        : {};
    const extSummary = Object.entries(extensions)
        .sort((a, b) => b[1] - a[1])
        .slice(0, 8)
        .map(([ext, count]) => `${count} .${ext}`)
        .join(', ');

    return `- Workspace indexed: ${ragIndex.files_count} files (${extSummary})`;
}

export async function buildCodingWorkspaceContext(args: BuildCodingWorkspaceContextArgs): Promise<CodingWorkspaceContext> {
    const projectPath = args.projectPath || '';
    const [codingRulesBlock, mentionAttachments] = projectPath
        ? await Promise.all([
            fetchCodingRules(projectPath),
            resolveContextMentions(args.userPrompt, projectPath),
        ])
        : ['', []];

    return {
        projectPath,
        localPath: args.localPath || undefined,
        remotePath: args.remotePath || undefined,
        projectContext: args.projectContext,
        gitBranch: args.gitBranch || undefined,
        gitSummary: args.gitSummary || undefined,
        fileImports: [...(args.fileImports || [])],
        ragSummary: formatWorkspaceRagSummary(args.ragIndex),
        codingRulesBlock: codingRulesBlock || '',
        mentionContextBlock: formatMentionAttachmentsForPrompt(mentionAttachments),
    };
}

export function buildSmartContextForWorkspace(
    workspace: CodingWorkspaceContext,
    args: BuildWorkspaceSmartContextArgs,
): SmartContext {
    return buildSmartContext(
        args.userPrompt,
        args.taskType,
        workspace.projectContext,
        workspace.gitSummary || null,
        args.agentMemory,
        workspace.fileImports,
        workspace.ragSummary,
        args.tokenBudget,
        args.budgetMode,
    );
}

export function buildCodingPlanPromptBlock(
    workspace: CodingWorkspaceContext,
    args: BuildCodingPlanPromptBlockArgs,
): string {
    if (!shouldRequestCodingPlanArtifact(workspace, args)) return '';

    const project = workspace.projectContext;
    const projectLabel = project
        ? [project.name, project.version ? `v${project.version}` : null].filter(Boolean).join(' ') || project.project_type
        : '';
    const workspaceFacts = [
        workspace.projectPath ? `- Workspace root: ${workspace.projectPath}` : null,
        workspace.localPath ? `- Current local path: ${workspace.localPath}` : null,
        workspace.remotePath ? `- Current remote path: ${workspace.remotePath}` : null,
        project ? `- Detected project: ${projectLabel} (${project.project_type})` : null,
        project && project.scripts.length > 0 ? `- Available scripts: ${project.scripts.slice(0, 8).join(', ')}` : null,
        workspace.gitBranch ? `- Git branch: ${workspace.gitBranch}` : null,
        workspace.fileImports.length > 0 ? `- Active editor imports: ${workspace.fileImports.slice(0, 8).join(', ')}` : null,
        workspace.ragSummary,
    ].filter((line): line is string => !!line);

    return [
        '<coding_plan_mode>',
        'Use this mode only for non-trivial coding work that may require edits, multiple files, risky changes, or verification.',
        'Do not emit a coding_plan for simple Q&A, direct explanations, file listings, or one obvious read-only action.',
        'A coding_plan is a review artifact only. Do not claim files were changed because of the plan, and do not treat the plan itself as approval to mutate files.',
        `Current approval mode: ${args.agentMode || 'normal'}. Keep later tool use compatible with that mode and the existing approval gates.`,
        '',
        'Workspace facts for planning:',
        workspaceFacts.length > 0 ? workspaceFacts.join('\n') : '- No additional workspace facts available.',
        '',
        'When a plan is appropriate, include exactly one fenced JSON block with info string "json coding_plan". Keep it concise and serializable:',
        '~~~json coding_plan',
        '{',
        '  "kind": "coding_plan",',
        '  "title": "Short plan title",',
        '  "summary": "One sentence describing the intended change.",',
        '  "riskLevel": "low|medium|high",',
        '  "scope": "single_file|multi_file|investigation|unknown",',
        '  "files": ["relative/path.ts"],',
        '  "steps": [',
        '    {"id": "1", "title": "Inspect the relevant code", "description": "What to learn before editing.", "files": ["relative/path.ts"]}',
        '  ],',
        '  "verification": ["npm run typecheck"],',
        '  "questions": [],',
        '  "warnings": []',
        '}',
        '~~~',
        'Limit plans to 3-6 steps. If you need clarification before a safe plan, put the question in "questions" and do not invent details.',
        '</coding_plan_mode>',
    ].join('\n');
}

export function workspaceToSystemPromptContext(
    workspace: CodingWorkspaceContext,
): Pick<SystemPromptContext,
    'localPath'
    | 'remotePath'
    | 'projectContext'
    | 'gitBranch'
    | 'gitSummary'
    | 'fileImports'
    | 'codingRulesBlock'
    | 'mentionContextBlock'
> {
    return {
        localPath: workspace.localPath,
        remotePath: workspace.remotePath,
        projectContext: workspace.projectContext,
        gitBranch: workspace.gitBranch,
        gitSummary: workspace.gitSummary,
        fileImports: workspace.fileImports,
        codingRulesBlock: workspace.codingRulesBlock || undefined,
        mentionContextBlock: workspace.mentionContextBlock || undefined,
    };
}
