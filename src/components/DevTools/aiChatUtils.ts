import { Message } from './aiChatTypes';
import { TaskType } from '../../types/ai';
import { computeResponseBuffer } from './aiChatTokenInfo';

// Rate limiter: tracks request timestamps per provider
const rateLimitMap = new Map<string, number[]>();
const RATE_LIMIT_RPM = 20; // max requests per minute per provider

export function checkRateLimit(providerId: string): { allowed: boolean; waitSeconds: number } {
    const now = Date.now();
    const windowMs = 60_000;
    const timestamps = (rateLimitMap.get(providerId) || []).filter(t => now - t < windowMs);
    rateLimitMap.set(providerId, timestamps);
    if (timestamps.length >= RATE_LIMIT_RPM) {
        const oldest = timestamps[0];
        const waitMs = windowMs - (now - oldest);
        return { allowed: false, waitSeconds: Math.ceil(waitMs / 1000) };
    }
    return { allowed: true, waitSeconds: 0 };
}

export function recordRequest(providerId: string) {
    const timestamps = rateLimitMap.get(providerId) || [];
    timestamps.push(Date.now());
    rateLimitMap.set(providerId, timestamps);
}

// Retry with exponential backoff
export async function withRetry<T>(
    fn: () => Promise<T>,
    maxAttempts: number = 3,
    baseDelayMs: number = 1000,
): Promise<T> {
    let lastError: unknown;
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
        try {
            return await fn();
        } catch (error: unknown) {
            lastError = error;
            const errStr = String(error).toLowerCase();
            // Only retry on transient errors (network, rate limit, server errors)
            const isRetryable = errStr.includes('rate limit') ||
                errStr.includes('timeout') ||
                errStr.includes('429') ||
                errStr.includes('500') ||
                errStr.includes('502') ||
                errStr.includes('503') ||
                errStr.includes('network') ||
                errStr.includes('fetch');
            if (!isRetryable || attempt === maxAttempts - 1) throw error;
            const delay = baseDelayMs * Math.pow(2, attempt);
            await new Promise(resolve => setTimeout(resolve, delay));
        }
    }
    throw lastError;
}

// Estimate token count for a string (~4 chars per token heuristic)
export function estimateTokens(text: string): number {
    return Math.ceil(text.length / 4);
}

// Build a context-aware message window within a token budget
export function buildMessageWindow(
    allMessages: Message[],
    systemPromptTokens: number,
    currentUserTokens: number,
    maxContextTokens: number,
    contextTokens: number = 0,  // tokens used by smart context
): { messages: Array<{ role: string; content: string }>; summarized: boolean; historyTokens: number } {
    // Reserve tokens: system prompt + current message + response buffer + smart context
    const responseBuffer = computeResponseBuffer(maxContextTokens);
    const availableTokens = maxContextTokens - systemPromptTokens - currentUserTokens - responseBuffer - contextTokens;

    if (availableTokens <= 0) {
        // Not enough budget — include only the last message, truncated to ~500 tokens
        const lastMessage = allMessages[allMessages.length - 1];
        if (!lastMessage) {
            return { messages: [], summarized: false, historyTokens: 0 };
        }
        const maxChars = 2000;
        const truncatedContent = lastMessage.content.length > maxChars
            ? lastMessage.content.slice(0, maxChars) + '\n[...truncated due to token limit]'
            : lastMessage.content;
        return {
            messages: [{
                role: lastMessage.role === 'user' ? 'user' : 'assistant',
                content: truncatedContent,
            }],
            summarized: false,
            historyTokens: estimateTokens(truncatedContent),
        };
    }

    // Walk backwards from most recent, accumulating tokens
    // Priority: user messages are preferred over assistant messages
    let usedTokens = 0;
    let lastIncludedIndex = allMessages.length;
    const truncatedIndices = new Set<number>();

    for (let i = allMessages.length - 1; i >= 0; i--) {
        const msg = allMessages[i];
        const msgTokens = estimateTokens(msg.content);
        if (usedTokens + msgTokens > availableTokens) {
            // If this is a recent user message (within last 4), try to include it by compressing
            const isRecentUser = msg.role === 'user' && (allMessages.length - i) <= 4;
            if (isRecentUser && usedTokens + Math.floor(msgTokens * 0.5) <= availableTokens) {
                // Include truncated version
                lastIncludedIndex = i;
                usedTokens += Math.floor(msgTokens * 0.5);
                truncatedIndices.add(i);
                continue;
            }
            lastIncludedIndex = i + 1;
            break;
        }
        usedTokens += msgTokens;
        lastIncludedIndex = i;
    }

    // If all messages fit, return them as-is
    if (lastIncludedIndex === 0) {
        return {
            messages: allMessages.map(m => ({
                role: m.role === 'user' ? 'user' : 'assistant',
                content: m.content,
            })),
            summarized: false,
            historyTokens: usedTokens,
        };
    }

    // Some messages were excluded — generate a summary placeholder
    const excludedMessages = allMessages.slice(0, lastIncludedIndex);
    let includedMessages = allMessages.slice(lastIncludedIndex);

    // Guarantee at least the last message is included (truncated if needed)
    if (includedMessages.length === 0 && allMessages.length > 0) {
        const lastMsg = allMessages[allMessages.length - 1];
        const maxChars = Math.max(availableTokens * 4, 200);
        const truncated = {
            ...lastMsg,
            content: lastMsg.content.length > maxChars
                ? lastMsg.content.slice(0, maxChars) + '\n[...truncated]'
                : lastMsg.content,
        };
        includedMessages = [truncated];
    }

    // Build a more informative summary of excluded messages
    const summaryParts: string[] = [];
    const userMsgCount = excludedMessages.filter(m => m.role === 'user').length;
    const assistantMsgCount = excludedMessages.filter(m => m.role === 'assistant').length;
    summaryParts.push(`Earlier conversation (${userMsgCount} user + ${assistantMsgCount} assistant messages)`);

    // Include key user requests from excluded messages
    const userRequests = excludedMessages.filter(m => m.role === 'user');
    if (userRequests.length > 0) {
        summaryParts.push('Key topics discussed:');
        userRequests.slice(-3).forEach(m => {
            const preview = m.content.slice(0, 80) + (m.content.length > 80 ? '...' : '');
            summaryParts.push(`- ${preview}`);
        });
    }

    const summaryText = summaryParts.join('\n');

    const result: Array<{ role: string; content: string }> = [];
    result.push({ role: 'assistant', content: summaryText });

    for (const m of includedMessages) {
        const msgIdx = allMessages.indexOf(m);
        const content = truncatedIndices.has(msgIdx)
            ? m.content.slice(0, Math.floor(m.content.length * 0.5)) + '\n[...truncated]'
            : m.content;
        result.push({
            role: m.role === 'user' ? 'user' : 'assistant',
            content,
        });
    }

    return { messages: result, summarized: true, historyTokens: usedTokens };
}

// Detect task type from user input for auto-routing
export function detectTaskType(input: string): TaskType {
    // Code generation patterns
    if (/\b(create|write|generate|build|implement|make|add)\b.*\b(function|class|component|code|file|script)\b/i.test(input) ||
        /\b(new|create)\b.*\b(file|folder|directory)\b/i.test(input)) {
        return 'code_generation';
    }

    // Code review patterns
    if (/\b(review|refactor|improve|optimize|fix|debug|check)\b.*\b(code|function|class|file)\b/i.test(input) ||
        /\bwhat('s| is)\b.*\b(wrong|issue|bug|problem)\b/i.test(input)) {
        return 'code_review';
    }

    // File analysis patterns
    if (/\b(read|show|display|analyze|explain|what)\b.*\b(file|content|code)\b/i.test(input) ||
        /\b(list|show|display)\b.*\b(files|folders|directory)\b/i.test(input)) {
        return 'file_analysis';
    }

    // Terminal command patterns
    if (/\b(run|execute|terminal|command|shell|bash|npm|git|chmod)\b/i.test(input) ||
        /\b(how to|how do i)\b.*\b(install|run|start|build)\b/i.test(input)) {
        return 'terminal_command';
    }

    // Quick answer patterns
    if (/^(what|how|why|when|where|who|is|are|can|could|would|should)\b/i.test(input) &&
        input.length < 100) {
        return 'quick_answer';
    }

    return 'general';
}

/** Parse multiple TOOL:/ARGS: blocks from AI response content */
export function parseToolCalls(content: string): Array<{ tool: string; args: Record<string, unknown> }> {
    const results: Array<{ tool: string; args: Record<string, unknown> }> = [];
    // Strip code fences and inline code before parsing to avoid matching examples in documentation
    const stripped = content.replace(/```[\s\S]*?```/g, '').replace(/`[^`]+`/g, '');
    // Use ^ anchor with multiline flag to only match TOOL: at the start of a line
    const toolRegex = /^TOOL:\s*(\w+)/gim;
    let match;
    while ((match = toolRegex.exec(stripped)) !== null) {
        const toolName = match[1];
        // Look for ARGS: after this match position (use stripped text since indices are relative to it)
        const afterMatch = stripped.slice(match.index + match[0].length);
        const argsMatch = afterMatch.match(/^\s*\n?\s*ARGS:\s*/i);
        let args: Record<string, unknown> = {};
        if (argsMatch) {
            const jsonStart = match.index + match[0].length + argsMatch.index! + argsMatch[0].length;
            const remaining = stripped.slice(jsonStart);
            // Brace-counting JSON extraction
            if (remaining.startsWith('{')) {
                let depth = 0;
                let endIdx = 0;
                for (let i = 0; i < remaining.length; i++) {
                    if (remaining[i] === '{') depth++;
                    else if (remaining[i] === '}') {
                        depth--;
                        if (depth === 0) { endIdx = i + 1; break; }
                    }
                }
                if (endIdx > 0) {
                    try {
                        args = JSON.parse(remaining.slice(0, endIdx));
                    } catch { /* ignore parse error, keep empty args */ }
                }
            }
        }
        results.push({ tool: toolName, args });
    }
    return results;
}

// Format tool result for display
export function formatToolResult(_toolName: string, result: unknown): string {
    if (result && typeof result === 'object') {
        const r = result as Record<string, unknown>;
        // List results
        if (r.entries && Array.isArray(r.entries)) {
            const entries = r.entries as Array<{ name: string; is_dir: boolean; size: number }>;
            const lines = entries.map(e => `${e.is_dir ? '/' : ' '} ${e.name}${e.is_dir ? '' : ` (${e.size} bytes)`}`);
            let output = lines.join('\n');
            if (r.truncated) output += `\n_...truncated (${r.total} total)_`;
            return `\`\`\`\n${output}\n\`\`\``;
        }
        // Read results
        if (typeof r.content === 'string') {
            let output = r.content as string;
            if (r.truncated) output += `\n\n_...truncated (${r.size} bytes total)_`;
            return `\`\`\`\n${output}\n\`\`\``;
        }
        // Sync preview results
        if (r.synced !== undefined) {
            const lines: string[] = [];
            lines.push(`**Local:** ${r.local_files} files | **Remote:** ${r.remote_files} files | **Identical:** ${r.identical}`);
            if (r.synced) {
                lines.push('\n**Folders are in sync.**');
            } else {
                const onlyLocal = r.only_local as Array<{ name: string; size: number }>;
                const onlyRemote = r.only_remote as Array<{ name: string; size: number }>;
                const sizeDiff = r.size_different as Array<{ name: string; local_size: number; remote_size: number }>;
                if (onlyLocal?.length) {
                    lines.push(`\n**Only local** (${onlyLocal.length}):`);
                    onlyLocal.forEach(f => lines.push(`  + ${f.name} (${f.size} bytes)`));
                }
                if (onlyRemote?.length) {
                    lines.push(`\n**Only remote** (${onlyRemote.length}):`);
                    onlyRemote.forEach(f => lines.push(`  - ${f.name} (${f.size} bytes)`));
                }
                if (sizeDiff?.length) {
                    lines.push(`\n**Size differs** (${sizeDiff.length}):`);
                    sizeDiff.forEach(f => lines.push(`  ~ ${f.name} (local: ${f.local_size}, remote: ${f.remote_size})`));
                }
            }
            return lines.join('\n');
        }
        // Batch upload/download results
        if (typeof r.uploaded === 'number' || typeof r.downloaded === 'number') {
            const count = (r.uploaded ?? r.downloaded) as number;
            const action = r.uploaded !== undefined ? 'Uploaded' : 'Downloaded';
            const files = r.files as string[] | undefined;
            const errors = r.errors as Array<{ file: string; error: string }> | undefined;
            const lines: string[] = [];
            lines.push(`**${action} ${count} file(s)**`);
            if (files?.length) lines.push(files.map(f => `  + ${f}`).join('\n'));
            if (errors?.length) {
                lines.push(`\n**Failed (${errors.length}):**`);
                errors.forEach(e => lines.push(`  - ${e.file}: ${e.error}`));
            }
            return lines.join('\n');
        }
        // Edit results
        if (r.replaced !== undefined) {
            return r.success
                ? `**Replaced ${r.replaced} occurrence(s)** in \`${(r.message as string | undefined)?.split(' in ').pop() || 'file'}\``
                : String(r.message || 'String not found in file');
        }
        // Batch move/copy/trash results (moved/copied/trashed + files + errors)
        if (typeof r.moved === 'number' || typeof r.copied === 'number' || typeof r.trashed === 'number') {
            const count = (r.moved ?? r.copied ?? r.trashed) as number;
            const action = r.moved !== undefined ? 'Moved' : r.copied !== undefined ? 'Copied' : 'Trashed';
            const files = r.files as string[] | undefined;
            const errors = r.errors as Array<{ file: string; error: string }> | undefined;
            const lines: string[] = [];
            lines.push(`**${action} ${count}/${r.total} file(s)**`);
            if (files?.length) lines.push(files.map(f => `  + ${f}`).join('\n'));
            if (errors?.length) {
                lines.push(`\n**Failed (${errors.length}):**`);
                errors.forEach(e => lines.push(`  - ${e.file}: ${e.error}`));
            }
            return lines.join('\n');
        }
        // Batch rename results
        if (typeof r.renamed === 'number' && Array.isArray(r.renames)) {
            const renames = r.renames as Array<{ from: string; to: string }>;
            const errors = r.errors as Array<{ file: string; error: string }> | undefined;
            const lines: string[] = [];
            lines.push(`**Renamed ${r.renamed}/${r.total} file(s)**`);
            if (renames.length) lines.push(renames.map(re => `  ${re.from} → ${re.to}`).join('\n'));
            if (errors?.length) {
                lines.push(`\n**Failed (${errors.length}):**`);
                errors.forEach(e => lines.push(`  - ${e.file}: ${e.error}`));
            }
            return lines.join('\n');
        }
        // File info results
        if (r.is_file !== undefined && r.name) {
            const lines: string[] = [];
            lines.push(`**${r.name}** — ${r.is_dir ? 'Directory' : `${r.size} bytes`}`);
            if (r.mime_type) lines.push(`Type: ${r.mime_type}`);
            if (r.permissions_octal) lines.push(`Permissions: ${r.permissions_octal}`);
            if (r.readonly) lines.push('Read-only');
            if (r.is_symlink) lines.push('Symlink');
            return lines.join(' | ');
        }
        // Disk usage results
        if (r.total_human && r.file_count !== undefined) {
            return `**${r.total_human}** — ${r.file_count} files, ${r.dir_count} dirs\n\`${r.path}\``;
        }
        // Find duplicates results
        if (r.duplicates && Array.isArray(r.duplicates)) {
            const dupes = r.duplicates as Array<{ hash: string; size: number; count: number; files: string[] }>;
            const lines: string[] = [];
            lines.push(`**${r.groups} duplicate group(s)** — ${r.total_wasted_human} wasted`);
            for (const g of dupes.slice(0, 10)) {
                lines.push(`\n${g.count}x (${g.size} bytes):`);
                g.files.forEach(f => lines.push(`  - ${f}`));
            }
            return lines.join('\n');
        }
        // RAG index results
        if (r.files_count !== undefined && r.extensions) {
            const exts = Object.entries(r.extensions as Record<string, number>)
                .sort(([, a], [, b]) => b - a).slice(0, 5)
                .map(([ext, count]) => `${ext}: ${count}`).join(', ');
            return `**Indexed ${r.files_count} files** (${r.dirs_count} dirs)\nExtensions: ${exts}`;
        }
        // RAG search results
        if (r.matches && Array.isArray(r.matches) && r.files_scanned !== undefined) {
            const matches = r.matches as Array<{ path: string; line: number; context: string }>;
            if (matches.length === 0) return `No matches for "${r.query}" (${r.files_scanned} files scanned)`;
            const lines: string[] = [];
            lines.push(`**${matches.length} match(es)** for "${r.query}" (${r.files_scanned} files scanned)`);
            for (const m of matches.slice(0, 20)) {
                lines.push(`  \`${m.path}:${m.line}\` — ${m.context}`);
            }
            return lines.join('\n');
        }
        // Hash file results
        if (r.hash && r.algorithm) {
            return `**${(r.algorithm as string).toUpperCase()}** \`${r.hash}\`\n\`${r.path}\``;
        }
        // Grep results (local_grep)
        if (r.total_matches !== undefined && r.files_searched !== undefined && Array.isArray(r.matches)) {
            const matches = r.matches as Array<{ file: string; line_number: number; line: string; context_before?: string[]; context_after?: string[] }>;
            if (matches.length === 0) return `No matches for \`${r.pattern}\` (${r.files_searched} files searched)`;
            const lines: string[] = [];
            lines.push(`**${r.total_matches} match(es)** in ${r.files_searched} files for \`${r.pattern}\``);
            for (const m of matches.slice(0, 30)) {
                lines.push(`  \`${m.file}:${m.line_number}\` — ${m.line}`);
            }
            if (matches.length > 30) lines.push(`  _...and ${matches.length - 30} more_`);
            return lines.join('\n');
        }
        // Head/tail results (local_head, local_tail)
        if (r.lines_read !== undefined && r.total_lines !== undefined && typeof r.content === 'string') {
            const label = r.file_name ? ` of \`${r.file_name}\`` : '';
            return `**${r.lines_read}/${r.total_lines} lines**${label}\n\`\`\`\n${r.content}\n\`\`\``;
        }
        // Stat batch results (local_stat_batch)
        if (r.files && Array.isArray(r.files) && r.total !== undefined && (r.files as Array<Record<string, unknown>>)[0]?.size_human !== undefined) {
            const files = r.files as Array<{ path: string; name: string; size_human?: string; modified?: string; is_file?: boolean; is_dir?: boolean; permissions?: string; exists: boolean }>;
            const lines: string[] = [];
            lines.push(`**${r.total} path(s)**`);
            for (const f of files) {
                if (!f.exists) {
                    lines.push(`  \u2717 \`${f.name}\` — not found`);
                } else {
                    const type_ = f.is_dir ? 'dir' : 'file';
                    lines.push(`  \`${f.name}\` — ${f.size_human || '0 B'} | ${type_} | ${f.permissions || ''} | ${f.modified || ''}`);
                }
            }
            return lines.join('\n');
        }
        // Diff results (local_diff)
        if (r.diff !== undefined && r.stats) {
            const stats = r.stats as { additions: number; deletions: number; file_a: string; file_b: string };
            if (r.identical) {
                return `**Files are identical:** \`${stats.file_a}\` = \`${stats.file_b}\``;
            }
            return `**+${stats.additions}/-${stats.deletions}** \`${stats.file_a}\` vs \`${stats.file_b}\`\n\`\`\`diff\n${r.diff}\n\`\`\``;
        }
        // Tree results (local_tree)
        if (typeof r.tree === 'string' && r.stats) {
            const stats = r.stats as { files: number; dirs: number; total_size_human: string };
            const truncNote = r.truncated ? ' _(truncated)_' : '';
            return `**${stats.files} files, ${stats.dirs} dirs** (${stats.total_size_human})${truncNote}\n\`\`\`\n${r.tree}\n\`\`\``;
        }
        // Shell execute results (shell_execute)
        if (r.exit_code !== undefined && (r.stdout !== undefined || r.stderr !== undefined)) {
            const lines: string[] = [];
            const cmd = r.command ? `\`${r.command}\`` : 'command';
            if (r.timed_out) {
                lines.push(`**${cmd}** — timed out`);
            } else {
                lines.push(`**${cmd}** — exit code ${r.exit_code}`);
            }
            const stdout = (r.stdout as string || '').trim();
            const stderr = (r.stderr as string || '').trim();
            if (stdout) {
                lines.push('```');
                lines.push(stdout);
                lines.push('```');
            }
            if (stderr) {
                lines.push(`**stderr:**\n\`\`\`\n${stderr}\n\`\`\``);
            }
            if (!stdout && !stderr && !r.timed_out) {
                lines.push('_(no output)_');
            }
            return lines.join('\n');
        }
        // Clipboard results
        if (r.length !== undefined && typeof r.content === 'string' && r.success) {
            const preview = (r.content as string).length > 500
                ? (r.content as string).slice(0, 500) + '...'
                : r.content as string;
            return `**Clipboard** (${r.length} chars)\n\`\`\`\n${preview}\n\`\`\``;
        }
        // Success message
        if (r.message) return String(r.message);
        // Search results (local_search returns name, is_dir, size)
        if (r.results && Array.isArray(r.results)) {
            const results = r.results as Array<{ name: string; path?: string; is_dir: boolean; size?: number }>;
            if (results.length === 0) return 'No results found.';
            const lines = results.map(e => {
                const display = e.path || e.name;
                return `${e.is_dir ? '/' : ' '} ${display}${!e.is_dir && e.size != null ? ` (${e.size} bytes)` : ''}`;
            });
            return `\`\`\`\n${lines.join('\n')}\n\`\`\``;
        }
    }
    return `\`\`\`json\n${JSON.stringify(result, null, 2)}\n\`\`\``;
}

/**
 * Parse a raw AI provider error into a user-friendly message + hint.
 * Extracts HTTP status code, provider error message, and selects an i18n hint.
 * Used by both streaming and non-streaming error handlers.
 */
export function formatProviderError(
    rawErr: string,
    t: (key: string) => string,
): string {
    let httpCode = 0;
    let friendlyMsg = '';

    // Extract HTTP status code
    const httpCodeMatch = rawErr.match(/HTTP (\d{3})/);
    if (httpCodeMatch) httpCode = parseInt(httpCodeMatch[1], 10);

    // Try to extract human-readable message from JSON error body
    const jsonMatch = rawErr.match(/\{[\s\S]*\}/);
    if (jsonMatch) {
        try {
            const parsed = JSON.parse(jsonMatch[0]);
            if (parsed?.error?.code && !httpCode) httpCode = parseInt(String(parsed.error.code), 10) || 0;

            // Extract the most useful message — check metadata.raw first (OpenRouter),
            // then error.message, then top-level message
            const metadataRaw = parsed?.error?.metadata?.raw;
            const errorMessage = parsed?.error?.message;
            const topMessage = parsed?.message;
            const statusMsg = parsed?.error?.status;

            if (metadataRaw && typeof metadataRaw === 'string') {
                friendlyMsg = metadataRaw;
            } else if (errorMessage && errorMessage !== 'Provider returned error') {
                friendlyMsg = errorMessage;
            } else if (topMessage) {
                friendlyMsg = topMessage;
            } else if (statusMsg) {
                friendlyMsg = statusMsg;
            } else if (errorMessage) {
                friendlyMsg = errorMessage;
            }
        } catch { /* keep empty — fallback below */ }
    }

    // Fallback: use raw error without JSON blob
    if (!friendlyMsg) {
        friendlyMsg = rawErr.replace(/\s*[\-—]\s*\{[\s\S]*\}/, '').trim() || rawErr;
    }

    // Sanitize file paths
    friendlyMsg = friendlyMsg.replace(/\/[\w\/./-]+/g, '[path]').replace(/\\[\w\\.\\-]+/g, '[path]');

    // Select hint based on HTTP status code, then text patterns
    let hint = '';
    const errLower = rawErr.toLowerCase();

    if (httpCode === 401 || httpCode === 403) {
        hint = t('ai.errorAuth');
    } else if (httpCode === 429) {
        hint = t('ai.errorRateLimit');
    } else if (httpCode === 404) {
        if (errLower.includes('tool use') || errLower.includes('tool_use') || errLower.includes('function calling')) {
            hint = t('ai.errorToolUse');
        } else {
            hint = t('ai.errorModelNotFound');
        }
    } else if (httpCode === 500 || httpCode === 502) {
        hint = t('ai.errorServerError');
    } else if (httpCode === 503) {
        hint = t('ai.errorServiceUnavailable');
    } else if (httpCode === 504) {
        hint = t('ai.errorTimeout');
    } else if (errLower.includes('tool use') || errLower.includes('tool_use') || errLower.includes('function calling')) {
        hint = t('ai.errorToolUse');
    } else if (errLower.includes('unauthorized') || errLower.includes('auth')) {
        hint = t('ai.errorAuth');
    } else if (errLower.includes('quota') || errLower.includes('rate limit')) {
        hint = t('ai.errorRateLimit');
    } else if (errLower.includes('network') || errLower.includes('fetch') || errLower.includes('timeout') || errLower.includes('connection')) {
        hint = t('ai.errorNetwork');
    } else if (errLower.includes('unavailable') || errLower.includes('503')) {
        hint = t('ai.errorServiceUnavailable');
    } else {
        hint = t('ai.errorDefault');
    }

    // Build HTTP label (e.g. "429 — ")
    const codeLabel = httpCode ? `${httpCode} — ` : '';

    return `**Error**: ${codeLabel}${friendlyMsg}\n\n${hint}`;
}
