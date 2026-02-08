/**
 * Intelligent retry strategies for AI tool failures.
 * Analyzes errors by tool type and suggests context-aware recovery actions.
 */

export interface RetryStrategy {
    /** Whether the error is retryable at all */
    canRetry: boolean;
    /** Human-readable suggestion for the user */
    suggestion: string;
    /** Tool name to suggest the AI use instead */
    suggestedTool?: string;
    /** Pre-filled args for the suggested tool */
    suggestedArgs?: Record<string, unknown>;
    /** If true, auto-retry with exponential backoff (transient errors only) */
    autoRetry?: boolean;
    /** Max retry attempts (default 3) */
    maxRetries?: number;
}

/**
 * Analyze a tool execution error and return an appropriate recovery strategy.
 */
export function analyzeToolError(
    toolName: string,
    args: Record<string, unknown>,
    error: string,
): RetryStrategy {
    const errLower = error.toLowerCase();

    // local_edit / remote_edit: "string not found"
    if ((toolName === 'local_edit' || toolName === 'remote_edit') &&
        (errLower.includes('not found') || errLower.includes('occurrences": 0') || errLower.includes('occurrences\": 0'))) {
        const searchQuery = String(args.find || '').slice(0, 100);
        return {
            canRetry: false,
            suggestion: `Search string not found in "${args.path}". Try rag_search to locate the exact text, or check for whitespace/encoding differences.`,
            suggestedTool: 'rag_search',
            suggestedArgs: { query: searchQuery, path: String(args.path || '') },
        };
    }

    // Not connected
    if (errLower.includes('not connected')) {
        return {
            canRetry: false,
            suggestion: 'Not connected to any server. Please establish a connection first.',
        };
    }

    // Permission denied
    if (errLower.includes('permission denied') || errLower.includes('access denied')) {
        return {
            canRetry: false,
            suggestion: `Permission denied for "${args.path || ''}". Check file permissions or try a different path.`,
        };
    }

    // File not found
    if (errLower.includes('no such file') || errLower.includes('file not found') || errLower.includes('not found') && errLower.includes('file')) {
        const pathStr = String(args.path || '');
        const parentPath = pathStr.split('/').slice(0, -1).join('/') || '/';
        const isRemote = toolName.startsWith('remote_');
        return {
            canRetry: false,
            suggestion: `File not found: "${pathStr}". Use ${isRemote ? 'remote_list' : 'local_list'} to verify the path.`,
            suggestedTool: isRemote ? 'remote_list' : 'local_list',
            suggestedArgs: { path: parentPath },
        };
    }

    // Rate limit / timeout - auto-retry
    if (errLower.includes('rate limit') || errLower.includes('timeout') ||
        errLower.includes('429') || errLower.includes('503') || errLower.includes('502')) {
        return {
            canRetry: true,
            autoRetry: true,
            maxRetries: 3,
            suggestion: 'Rate limited or timeout. Retrying automatically...',
        };
    }

    // Disk space
    if (errLower.includes('no space') || errLower.includes('disk full') || errLower.includes('quota')) {
        return {
            canRetry: false,
            suggestion: 'Disk full or quota exceeded. Free up space and try again.',
        };
    }

    // File too large
    if (errLower.includes('too large') || errLower.includes('size limit')) {
        return {
            canRetry: false,
            suggestion: `File is too large for this operation. Consider working with smaller files.`,
        };
    }

    // UTF-8 encoding error
    if (errLower.includes('utf-8') || errLower.includes('utf8') || errLower.includes('valid text')) {
        return {
            canRetry: false,
            suggestion: 'File is not valid UTF-8 text. It may be a binary file.',
        };
    }

    // Default: non-retryable with generic message
    return {
        canRetry: false,
        suggestion: `Tool "${toolName}" failed: ${error}`,
    };
}
