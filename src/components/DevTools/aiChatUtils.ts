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
        // Success message
        if (r.message) return String(r.message);
        // Search results
        if (r.results && Array.isArray(r.results)) {
            const results = r.results as Array<{ name: string; path: string; is_dir: boolean }>;
            return results.map(e => `${e.is_dir ? '/' : ' '} ${e.path}`).join('\n') || 'No results found.';
        }
    }
    return `\`\`\`json\n${JSON.stringify(result, null, 2)}\n\`\`\``;
}
