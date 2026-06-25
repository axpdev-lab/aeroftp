// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { AIProviderType } from '../../types/ai';
import type { EffectiveTheme } from '../../hooks/useTheme';

/** Agent autonomy mode: safe → normal → expert → extreme */
export type AgentMode = 'safe' | 'normal' | 'expert' | 'extreme';

/** Max multi-step iterations per agent mode */
export const AGENT_MODE_MAX_STEPS: Record<AgentMode, number> = {
    safe: 5,
    normal: 10,
    expert: 25,
    extreme: 50,
};

// Vision constants
export const MAX_IMAGE_SIZE = 20 * 1024 * 1024; // 20 MB
export const MAX_IMAGES = 5;
export const SUPPORTED_IMAGE_TYPES = ['image/jpeg', 'image/png', 'image/gif', 'image/webp'];
export const MAX_DIMENSION = 2048;

export interface VisionImage {
    data: string;       // base64 (no data URI prefix)
    mediaType: string;  // "image/jpeg" etc.
    preview: string;    // "data:image/jpeg;base64,..." for local display
}

export interface TransferPlanOperation {
    id: string;
    toolName: string;
    title: string;
    description: string;
    category: 'prepare' | 'upload' | 'download';
    dangerLevel: 'safe' | 'medium' | 'high';
    args: Record<string, unknown>;
    dependsOn?: string[];
}

export interface TransferPlan {
    direction: 'upload' | 'download';
    destination: string;
    source_count: number;
    operation_count: number;
    executable_operations: number;
    summary: string;
    warnings: string[];
    operations: TransferPlanOperation[];
}

export interface TransferPlanResultData {
    kind: 'transfer_plan';
    plan: TransferPlan;
}

export type CodingPlanRiskLevel = 'low' | 'medium' | 'high';
export type CodingPlanScope = 'single_file' | 'multi_file' | 'investigation' | 'unknown';

export interface CodingPlanStep {
    id: string;
    title: string;
    description?: string;
    files?: string[];
}

export interface CodingPlanArtifact {
    kind: 'coding_plan';
    title: string;
    summary: string;
    riskLevel: CodingPlanRiskLevel;
    scope: CodingPlanScope;
    files: string[];
    steps: CodingPlanStep[];
    verification: string[];
    questions: string[];
    warnings: string[];
}

export interface CodingPlanResultData {
    kind: 'coding_plan';
    plan: CodingPlanArtifact;
}

export interface CodingPatchFileResult {
    path: string;
    status: string;
    hunks: number;
    additions: number;
    deletions: number;
    old_size_bytes: number;
    new_size_bytes: number;
}

export interface CodingPatchDiagnostic {
    path?: string | null;
    hunk_index?: number | null;
    message: string;
    expected?: string | null;
    actual?: string | null;
}

export interface CodingPatchResult {
    success: boolean;
    dry_run: boolean;
    checkpoint_id?: string | null;
    files: CodingPatchFileResult[];
    diagnostics: CodingPatchDiagnostic[];
    warnings: string[];
}

export interface CodingPatchResultData {
    kind: 'coding_patch';
    result: CodingPatchResult;
    workspaceRoot?: string;
    patch?: string;
}

export interface CodingCheckpointRestoreFileResult {
    path: string;
    action: string;
    existed_at_checkpoint: boolean;
    size_bytes: number;
    sha256?: string | null;
}

export interface CodingCheckpointRestoreResult {
    checkpoint_id: string;
    workspace_root: string;
    dry_run: boolean;
    files: CodingCheckpointRestoreFileResult[];
}

export interface CodingCheckpointRestoreResultData {
    kind: 'coding_checkpoint_restore';
    result: CodingCheckpointRestoreResult;
    requestedPaths?: string[];
}

export interface CodingGitFileStatus {
    path: string;
    index_status: string;
    worktree_status: string;
}

export interface CodingGitStatusResult {
    workspace_root: string;
    repo_root: string;
    branch?: string | null;
    head?: string | null;
    upstream?: string | null;
    ahead: number;
    behind: number;
    clean: boolean;
    staged: CodingGitFileStatus[];
    unstaged: CodingGitFileStatus[];
    untracked: CodingGitFileStatus[];
    conflicted: CodingGitFileStatus[];
    total: number;
    truncated: boolean;
    raw: string[];
}

export interface CodingGitDiffStat {
    path: string;
    additions: number;
    deletions: number;
    binary: boolean;
}

export interface CodingGitDiffResult {
    workspace_root: string;
    repo_root: string;
    staged: boolean;
    paths: string[];
    file_count: number;
    total_additions: number;
    total_deletions: number;
    stats: CodingGitDiffStat[];
    diff: string;
    truncated: boolean;
}

export interface CodingGitStageResult {
    success: boolean;
    workspace_root: string;
    repo_root: string;
    dry_run: boolean;
    staged: boolean;
    paths: string[];
    before: CodingGitStatusResult;
    after?: CodingGitStatusResult | null;
    message: string;
}

export interface CodingGitCommitResult {
    success: boolean;
    workspace_root: string;
    repo_root: string;
    dry_run: boolean;
    committed: boolean;
    commit_hash?: string | null;
    message: string;
    stdout: string;
    stderr: string;
    before: CodingGitStatusResult;
    after?: CodingGitStatusResult | null;
}

export type CodingGitResult =
    | CodingGitStatusResult
    | CodingGitDiffResult
    | CodingGitStageResult
    | CodingGitCommitResult;

export interface CodingGitResultData {
    kind: 'coding_git';
    toolName: 'coding_git_status' | 'coding_git_diff' | 'coding_git_stage' | 'coding_git_commit';
    result: CodingGitResult;
    requestedPaths?: string[];
    commitMessage?: string;
}

export interface CodingRunCheckResult {
    success: boolean;
    workspace_root: string;
    check: string;
    label: string;
    program: string;
    args: string[];
    filter?: string | null;
    exit_code?: number | null;
    timed_out: boolean;
    timeout_secs: number;
    duration_ms: number;
    stdout: string;
    stderr: string;
    stdout_truncated: boolean;
    stderr_truncated: boolean;
}

export interface CodingRunCheckResultData {
    kind: 'coding_run_checks';
    result: CodingRunCheckResult;
}

export interface CodingVerifyResult {
    workspace_root: string;
    overall_success: boolean;
    stopped_early: boolean;
    checks: CodingRunCheckResult[];
}

export interface CodingVerifyResultData {
    kind: 'coding_verify';
    result: CodingVerifyResult;
}

export interface CodingGitLogEntry {
    hash: string;
    short_hash: string;
    author: string;
    date: string;
    subject: string;
}

export interface CodingGitLogResult {
    workspace_root: string;
    repo_root: string;
    paths: string[];
    max_count: number;
    commits: CodingGitLogEntry[];
    truncated: boolean;
}

export interface CodingGitShowResult {
    workspace_root: string;
    repo_root: string;
    commit: string;
    hash: string;
    short_hash: string;
    author: string;
    date: string;
    subject: string;
    body: string;
    stats: CodingGitDiffStat[];
    total_additions: number;
    total_deletions: number;
    diff: string;
    truncated: boolean;
}

export type CodingGitHistoryResult = CodingGitLogResult | CodingGitShowResult;

export interface CodingGitHistoryResultData {
    kind: 'coding_git_history';
    toolName: 'coding_git_log' | 'coding_git_show';
    result: CodingGitHistoryResult;
}

export interface CodingDiagnostic {
    file?: string | null;
    line?: number | null;
    column?: number | null;
    severity: string;
    code?: string | null;
    message: string;
}

export interface CodingDiagnosticsResult {
    workspace_root: string;
    source: string;
    program: string;
    args: string[];
    exit_code?: number | null;
    timed_out: boolean;
    timeout_secs: number;
    duration_ms: number;
    success: boolean;
    error_count: number;
    warning_count: number;
    diagnostics: CodingDiagnostic[];
    truncated: boolean;
}

export interface CodingDiagnosticsResultData {
    kind: 'coding_diagnostics';
    result: CodingDiagnosticsResult;
}

export interface CodingSearchSubmatch {
    start: number;
    end: number;
    text: string;
}

export interface CodingSearchMatch {
    file: string;
    line: number;
    column: number;
    line_text: string;
    submatches: CodingSearchSubmatch[];
}

export interface CodingSearchResult {
    workspace_root: string;
    pattern: string;
    path?: string | null;
    globs: string[];
    case_insensitive: boolean;
    fixed_strings: boolean;
    program: string;
    args: string[];
    exit_code?: number | null;
    timed_out: boolean;
    timeout_secs: number;
    duration_ms: number;
    total_matches: number;
    file_count: number;
    matches: CodingSearchMatch[];
    truncated: boolean;
}

export interface CodingSearchResultData {
    kind: 'coding_search';
    result: CodingSearchResult;
}

export type ChatResultData =
    | TransferPlanResultData
    | CodingPlanResultData
    | CodingPatchResultData
    | CodingCheckpointRestoreResultData
    | CodingGitResultData
    | CodingRunCheckResultData
    | CodingGitHistoryResultData
    | CodingVerifyResultData
    | CodingDiagnosticsResultData
    | CodingSearchResultData;

export interface Message {
    id: string;
    role: 'user' | 'assistant';
    content: string;
    timestamp: Date;
    images?: VisionImage[];
    thinking?: string;
    thinkingDuration?: number;
    webSearchUsed?: boolean;
    toolName?: string;
    modelInfo?: {
        modelName: string;
        providerName: string;
        providerType: AIProviderType;
    };
    tokenInfo?: {
        inputTokens?: number;
        outputTokens?: number;
        totalTokens?: number;
        cost?: number;
        cacheCreationTokens?: number;  // Anthropic: tokens to create cache entry
        cacheReadTokens?: number;      // Anthropic: tokens read from cache (90% cheaper)
        cacheSavings?: number;         // Estimated USD savings from caching
    };
    toolResultData?: ChatResultData;
}

export interface AIChatProps {
    className?: string;
    remotePath?: string;
    localPath?: string;
    /** App-level theme for styling */
    appTheme?: EffectiveTheme;
    /** Active protocol type (e.g. 'sftp', 'ftp', 'googledrive') */
    providerType?: string;
    /** Whether currently connected to remote */
    isConnected?: boolean;
    /** Currently selected files in the file panel */
    selectedFiles?: string[];
    /** Server hostname for connection context */
    serverHost?: string;
    /** Server port for connection context */
    serverPort?: number;
    /** Username for connection context */
    serverUser?: string;
    /** Which file panel is currently active/focused */
    activeFilePanel?: 'remote' | 'local';
    /** Whether the connection is via AeroCloud (vs manual server) */
    isCloudConnection?: boolean;
    /** Callback to refresh file panels after AI tool mutations */
    onFileMutation?: (target: 'remote' | 'local' | 'both') => void;
    /** Currently open file name in the code editor */
    editorFileName?: string;
    /** Currently open file path in the code editor */
    editorFilePath?: string;
}

// Selected model state
export interface SelectedModel {
    providerId: string;
    providerName: string;
    providerType: AIProviderType;
    modelId: string;
    modelName: string;
    displayName: string;
}

// Tool names that mutate the filesystem and should trigger a panel refresh
export const MUTATION_TOOLS: Record<string, 'remote' | 'local' | 'both'> = {
    remote_delete: 'remote', remote_rename: 'remote', remote_mkdir: 'remote',
    remote_upload: 'remote', remote_edit: 'remote', upload_files: 'remote',
    download_files: 'local', remote_download: 'local',
    local_write: 'local', local_delete: 'local', local_rename: 'local', local_move_files: 'local',
    local_batch_rename: 'local', local_copy_files: 'local', local_trash: 'local',
    local_mkdir: 'local', local_edit: 'local',
    coding_apply_patch: 'local', coding_checkpoint_restore: 'local',
    archive_compress: 'both', archive_decompress: 'both',
};
