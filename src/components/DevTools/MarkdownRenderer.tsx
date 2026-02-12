import React, { useMemo, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import type { Components } from 'react-markdown';
import Prism from 'prismjs';

// Prism core languages (same set as FilePreview.tsx — loaded once via Vite)
import 'prismjs/components/prism-markup';
import 'prismjs/components/prism-clike';
import 'prismjs/components/prism-markup-templating';
import 'prismjs/components/prism-javascript';
import 'prismjs/components/prism-typescript';
import 'prismjs/components/prism-jsx';
import 'prismjs/components/prism-tsx';
import 'prismjs/components/prism-css';
import 'prismjs/components/prism-scss';
import 'prismjs/components/prism-json';
import 'prismjs/components/prism-markdown';
import 'prismjs/components/prism-php';
import 'prismjs/components/prism-python';
import 'prismjs/components/prism-bash';
import 'prismjs/components/prism-sql';
import 'prismjs/components/prism-yaml';
import 'prismjs/components/prism-rust';
import 'prismjs/components/prism-go';
import 'prismjs/components/prism-java';
import 'prismjs/components/prism-c';
import 'prismjs/components/prism-cpp';
import 'prismjs/components/prism-toml';
import 'prismjs/components/prism-docker';

import { CodeBlockActions } from './CodeBlockActions';
import { getToolByName } from '../../types/tools';
import { useI18n } from '../../i18n';
import { getToolLabel } from './aiChatToolLabels';

/** Escape HTML special characters to prevent XSS in unrecognized code blocks */
function escapeHtml(text: string): string {
    return text
        .replace(/&/g, '&amp;')
        .replace(/</g, '&lt;')
        .replace(/>/g, '&gt;')
        .replace(/"/g, '&quot;')
        .replace(/'/g, '&#039;');
}

/** Strip Unicode bidi override characters to prevent Trojan Source attacks (SEC-P4-004) */
function stripBidiChars(s: string): string {
    return s.replace(/[\u202A-\u202E\u2066-\u2069\u200E\u200F]/g, '');
}

// ── Tool Chip (React component, no dangerouslySetInnerHTML) ──────────

interface ToolChipProps {
    toolName: string;
    argsJson: string;
}

const ToolChip: React.FC<ToolChipProps> = ({ toolName, argsJson }) => {
    const { t } = useI18n();
    const label = getToolLabel(toolName, t);
    const isPlugin = !getToolByName(toolName);
    const borderColor = isPlugin ? 'border-cyan-500' : 'border-purple-500';
    const iconColor = isPlugin ? 'text-cyan-400' : 'text-purple-400';
    const icon = isPlugin ? '\u{1F9E9}' : '\u2699'; // puzzle vs gear

    let detail = '';
    try {
        const args = JSON.parse(argsJson);
        const path = args.path || args.remote_path || args.local_path || '';
        if (args.local_path && args.remote_path) {
            detail = `${args.local_path} \u2194 ${args.remote_path}`;
        } else if (path) {
            detail = String(path);
        }
    } catch { /* ignore malformed JSON */ }

    return (
        <div className={`inline-flex items-center gap-1.5 bg-gray-700 rounded-md px-2.5 py-0.5 my-1 text-xs border-l-[3px] ${borderColor}`}>
            <span className={iconColor}>{icon}</span>
            <strong>{label}</strong>
            {detail && <span className="opacity-70 ml-1.5">{detail}</span>}
        </div>
    );
};

// ── Content splitter ─────────────────────────────────────────────────

interface ContentSegment {
    type: 'markdown' | 'toolchip';
    content: string;
    toolName?: string;
    argsJson?: string;
}

/** Split content into alternating markdown text and TOOL/ARGS blocks */
function splitContent(text: string): ContentSegment[] {
    const segments: ContentSegment[] = [];
    const toolPattern = /TOOL:\s*(\w+)\s*\n\s*ARGS:\s*/gi;
    let lastIndex = 0;
    let match;

    while ((match = toolPattern.exec(text)) !== null) {
        // Add markdown text before this tool block
        if (match.index > lastIndex) {
            const md = text.slice(lastIndex, match.index).trim();
            if (md) segments.push({ type: 'markdown', content: md });
        }

        const toolName = match[1];
        const jsonStart = match.index + match[0].length;

        // Brace-counting to find complete JSON
        let depth = 0;
        let jsonEnd = jsonStart;
        for (let i = jsonStart; i < text.length; i++) {
            if (text[i] === '{') depth++;
            else if (text[i] === '}') {
                depth--;
                if (depth === 0) { jsonEnd = i + 1; break; }
            }
        }

        let argsJson = '{}';
        if (jsonEnd > jsonStart) {
            const candidate = text.slice(jsonStart, jsonEnd);
            try {
                JSON.parse(candidate);
                argsJson = candidate;
            } catch { /* use empty args */ }
        }

        segments.push({
            type: 'toolchip',
            content: match[0] + argsJson,
            toolName,
            argsJson,
        });
        lastIndex = jsonEnd;
        toolPattern.lastIndex = jsonEnd;
    }

    // Remaining markdown after last tool block
    if (lastIndex < text.length) {
        const md = text.slice(lastIndex).trim();
        if (md) segments.push({ type: 'markdown', content: md });
    }

    return segments.length > 0 ? segments : [{ type: 'markdown', content: text }];
}

// ── Incremental streaming split ──────────────────────────────────────

interface FinalizedSplit {
    finalized: string[];
    inProgress: string;
}

/**
 * Split markdown content into "finalized" segments that won't change,
 * and an "in-progress" tail that is still being streamed.
 *
 * Safe split points:
 *  - After a closed code block (``` ... ```)
 *  - After a paragraph boundary (\n\n)
 *  - After a complete heading line (\n# ...)
 *
 * During non-streaming, everything is a single finalized segment.
 */
function splitIntoFinalized(content: string, isStreaming: boolean): FinalizedSplit {
    if (!isStreaming) {
        return { finalized: [content], inProgress: '' };
    }

    // Find all safe split points by scanning for complete code blocks
    // and paragraph boundaries.
    const finalized: string[] = [];
    let cursor = 0;

    // Strategy: walk through the content, identifying complete blocks.
    // A code block starts with ``` at the beginning of a line and ends
    // with ``` at the beginning of another line.
    // A paragraph boundary is \n\n outside of a code block.

    while (cursor < content.length) {
        // Check if we're at the start of a code block
        const codeBlockStart = findCodeBlockStart(content, cursor);

        if (codeBlockStart !== -1 && codeBlockStart === cursor) {
            // We're at a code block opening. Find its close.
            const closeIdx = findCodeBlockClose(content, codeBlockStart);
            if (closeIdx === -1) {
                // Unclosed code block — everything from cursor onward is in-progress
                break;
            }
            // Found a complete code block. Find the end of the closing ``` line.
            const lineEnd = content.indexOf('\n', closeIdx);
            const blockEnd = lineEnd === -1 ? content.length : lineEnd + 1;

            // Include any text between the previous finalized end and this block start,
            // plus the block itself as one finalized segment.
            const segment = content.slice(cursor, blockEnd);
            if (segment.trim()) {
                finalized.push(segment);
            }
            cursor = blockEnd;
            continue;
        }

        // Look for the next paragraph boundary (\n\n) or code block start,
        // whichever comes first.
        const nextCodeBlock = findCodeBlockStart(content, cursor);
        const nextParaBreak = content.indexOf('\n\n', cursor);

        // Determine the next safe split point
        let splitAt = -1;

        if (nextParaBreak !== -1 && (nextCodeBlock === -1 || nextParaBreak < nextCodeBlock)) {
            // Paragraph break comes first
            splitAt = nextParaBreak + 2; // after \n\n
        } else if (nextCodeBlock !== -1 && nextCodeBlock > cursor) {
            // There's a code block ahead. Finalize everything before it as a paragraph chunk
            // only if there's a paragraph break before the code block.
            const paraBeforeCode = content.lastIndexOf('\n\n', nextCodeBlock);
            if (paraBeforeCode >= cursor) {
                splitAt = paraBeforeCode + 2;
            } else {
                // No paragraph break before the code block. Jump to the code block.
                cursor = nextCodeBlock;
                continue;
            }
        }

        if (splitAt === -1 || splitAt <= cursor) {
            // No more safe split points found — rest is in-progress
            break;
        }

        const segment = content.slice(cursor, splitAt);
        if (segment.trim()) {
            finalized.push(segment);
        }
        cursor = splitAt;
    }

    const inProgress = cursor < content.length ? content.slice(cursor) : '';
    return { finalized, inProgress };
}

/** Find the start of a code fence (```) at or after `from` that begins at a line start. */
function findCodeBlockStart(content: string, from: number): number {
    let idx = from;
    while (idx < content.length) {
        const fenceIdx = content.indexOf('```', idx);
        if (fenceIdx === -1) return -1;
        // Must be at the start of a line (or the very start of the string)
        if (fenceIdx === 0 || content[fenceIdx - 1] === '\n') {
            return fenceIdx;
        }
        idx = fenceIdx + 3;
    }
    return -1;
}

/** Find the closing ``` fence for a code block that opens at `openIdx`. */
function findCodeBlockClose(content: string, openIdx: number): number {
    // Skip past the opening fence line
    const lineEnd = content.indexOf('\n', openIdx);
    if (lineEnd === -1) return -1;
    let idx = lineEnd + 1;

    while (idx < content.length) {
        const fenceIdx = content.indexOf('```', idx);
        if (fenceIdx === -1) return -1;
        // Must be at the start of a line
        if (fenceIdx === 0 || content[fenceIdx - 1] === '\n') {
            return fenceIdx;
        }
        idx = fenceIdx + 3;
    }
    return -1;
}

// ── Language utilities ───────────────────────────────────────────────

/** Map language aliases to Prism grammar names */
const LANG_MAP: Record<string, string> = {
    js: 'javascript', ts: 'typescript', py: 'python', rb: 'ruby',
    sh: 'bash', shell: 'bash', zsh: 'bash', yml: 'yaml',
    rs: 'rust', dockerfile: 'docker', make: 'makefile',
    htm: 'markup', html: 'markup', xml: 'markup', svg: 'markup',
};

function normalizeLang(lang: string): string {
    const lower = lang.toLowerCase();
    return LANG_MAP[lower] || lower;
}

// ── Markdown components ──────────────────────────────────────────────

interface MarkdownRendererProps {
    content: string;
    isStreaming?: boolean;
    editorFilePath?: string;
    editorFileName?: string;
}

/** Build react-markdown custom components with code block actions */
function useMarkdownComponents(editorFilePath?: string, editorFileName?: string): Components {
    return useMemo(() => ({
        // Code block / inline code
        code({ className, children, ...props }) {
            const match = /language-(\w+)/.exec(className || '');
            const rawCode = String(children).replace(/\n$/, '');

            // Inline code (no language class, short content, no newlines)
            if (!match && !rawCode.includes('\n')) {
                return (
                    <code className="bg-gray-700 px-1 rounded text-purple-300 text-xs" {...props}>
                        {children}
                    </code>
                );
            }

            // Code block with syntax highlighting
            const lang = match ? normalizeLang(match[1]) : '';
            const grammar = lang && Prism.languages[lang] ? Prism.languages[lang] : null;
            // Strip bidi override characters to prevent Trojan Source attacks (SEC-P4-004)
            const safeCode = stripBidiChars(rawCode);
            // Prism.highlight handles HTML escaping internally for matched tokens.
            // For unrecognized languages (no grammar), we must escape manually to prevent XSS.
            const highlighted = grammar
                ? Prism.highlight(safeCode, grammar, lang)
                : escapeHtml(safeCode);

            return (
                <div className="relative group my-2">
                    {/* Language badge */}
                    {lang && (
                        <div className="absolute top-0 left-0 px-2 py-0.5 text-[10px] font-mono text-gray-400 bg-gray-800/80 rounded-br select-none">
                            {lang}
                        </div>
                    )}
                    <pre className="bg-gray-900 rounded p-3 pt-6 overflow-x-auto text-xs leading-relaxed">
                        <code
                            className={lang ? `language-${lang}` : undefined}
                            dangerouslySetInnerHTML={{ __html: highlighted }}
                        />
                    </pre>
                    {/* Action buttons (visible on hover) — rendered after <pre> so DiffPreview flows naturally */}
                    <CodeBlockActions
                        code={rawCode}
                        language={lang}
                        editorFilePath={editorFilePath}
                        editorFileName={editorFileName}
                    />
                </div>
            );
        },
        // Links — open in external browser (SEC-010: validate href scheme)
        a({ href, children }) {
            const safeHref = href && /^(https?:|mailto:|#)/.test(href) ? href : undefined;
            return (
                <a href={safeHref} target="_blank" rel="noopener noreferrer" className="text-purple-400 hover:text-purple-300 underline">
                    {children}
                </a>
            );
        },
        // Tables
        table({ children }) {
            return (
                <div className="overflow-x-auto my-2">
                    <table className="min-w-full text-xs border-collapse border border-gray-700">
                        {children}
                    </table>
                </div>
            );
        },
        th({ children }) {
            return <th className="border border-gray-700 bg-gray-800 px-2 py-1 text-left font-medium">{children}</th>;
        },
        td({ children }) {
            return <td className="border border-gray-700 px-2 py-1">{children}</td>;
        },
        // Blockquote
        blockquote({ children }) {
            return <blockquote className="border-l-2 border-purple-500 pl-3 my-2 text-gray-400 italic">{children}</blockquote>;
        },
        // Headings
        h1({ children }) { return <h1 className="text-lg font-bold text-white mt-3 mb-1">{children}</h1>; },
        h2({ children }) { return <h2 className="text-base font-bold text-white mt-3 mb-1">{children}</h2>; },
        h3({ children }) { return <h3 className="text-sm font-bold text-white mt-2 mb-1">{children}</h3>; },
        h4({ children }) { return <h4 className="text-sm font-semibold text-gray-200 mt-2 mb-1">{children}</h4>; },
        // Horizontal rule
        hr() { return <hr className="border-gray-700 my-3" />; },
        // List styling
        ul({ children }) { return <ul className="list-disc list-inside my-1 space-y-0.5">{children}</ul>; },
        ol({ children }) { return <ol className="list-decimal list-inside my-1 space-y-0.5">{children}</ol>; },
        li({ children }) { return <li className="text-gray-300">{children}</li>; },
        // Paragraphs
        p({ children }) { return <p className="my-1">{children}</p>; },
        // Strong / em
        strong({ children }) { return <strong className="font-semibold text-white">{children}</strong>; },
        em({ children }) { return <em className="italic">{children}</em>; },
    }), [editorFilePath, editorFileName]);
}

// ── Finalized segment renderer (stable key, no re-renders) ──────────

interface FinalizedSegmentProps {
    markdown: string;
    components: Components;
}

/**
 * Renders a finalized markdown segment. Wrapped in React.memo so it
 * never re-renders once its markdown string is set.
 */
const FinalizedSegment = React.memo<FinalizedSegmentProps>(
    ({ markdown, components }) => (
        <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
            {markdown}
        </ReactMarkdown>
    ),
    (prev, next) => prev.markdown === next.markdown,
);

// ── Main component ───────────────────────────────────────────────────

const MarkdownRendererInner: React.FC<MarkdownRendererProps> = ({
    content,
    isStreaming = false,
    editorFilePath,
    editorFileName,
}) => {
    const components = useMarkdownComponents(editorFilePath, editorFileName);

    // Split content into markdown segments and tool chip segments
    const segments = useMemo(() => splitContent(content), [content]);

    // Ref to cache previously rendered finalized segment counts per tool-split segment.
    // Key: segment index, Value: number of finalized segments previously seen.
    const prevFinalizedRef = useRef<Map<number, number>>(new Map());

    // For user messages or very short content, render simply
    if (!content) return null;

    // When streaming, we need to identify which markdown segment is the "last" one
    // (the one actively receiving tokens) and apply incremental rendering to it.
    const lastMarkdownIdx = isStreaming
        ? findLastMarkdownIndex(segments)
        : -1;

    return (
        <div className={`markdown-renderer text-sm text-gray-300 ${isStreaming ? 'streaming' : ''}`}>
            {segments.map((seg, i) => {
                if (seg.type === 'toolchip' && seg.toolName) {
                    return (
                        <ToolChip
                            key={`tool-${i}`}
                            toolName={seg.toolName}
                            argsJson={seg.argsJson || '{}'}
                        />
                    );
                }

                // For the last markdown segment during streaming, use incremental rendering
                if (isStreaming && i === lastMarkdownIdx) {
                    return (
                        <StreamingSegment
                            key={`md-stream-${i}`}
                            content={seg.content}
                            components={components}
                            segmentIndex={i}
                            prevFinalizedRef={prevFinalizedRef}
                        />
                    );
                }

                // Non-streaming or non-tail segments: render normally
                return (
                    <ReactMarkdown
                        key={`md-${i}`}
                        remarkPlugins={[remarkGfm]}
                        components={components}
                    >
                        {seg.content}
                    </ReactMarkdown>
                );
            })}
            {/* Streaming cursor indicator — only on the in-progress tail */}
            {isStreaming && (
                <span className="inline-block w-1.5 h-4 bg-purple-400 animate-pulse ml-0.5 align-text-bottom rounded-sm" />
            )}
        </div>
    );
};

/** Find the index of the last markdown segment in the list */
function findLastMarkdownIndex(segments: ContentSegment[]): number {
    for (let i = segments.length - 1; i >= 0; i--) {
        if (segments[i].type === 'markdown') return i;
    }
    return -1;
}

// ── Streaming segment with finalized/in-progress split ───────────────

interface StreamingSegmentProps {
    content: string;
    components: Components;
    segmentIndex: number;
    prevFinalizedRef: React.MutableRefObject<Map<number, number>>;
}

/**
 * During streaming, splits content into finalized segments (rendered once
 * with stable keys) and an in-progress tail (re-rendered on each chunk).
 */
const StreamingSegment: React.FC<StreamingSegmentProps> = ({
    content,
    components,
    segmentIndex,
    prevFinalizedRef,
}) => {
    const { finalized, inProgress } = useMemo(
        () => splitIntoFinalized(content, true),
        [content],
    );

    // Track how many finalized segments we've seen for this segment index.
    // This ensures stable keys: once a segment is finalized, its key never changes.
    const prevCount = prevFinalizedRef.current.get(segmentIndex) ?? 0;
    if (finalized.length > prevCount) {
        prevFinalizedRef.current.set(segmentIndex, finalized.length);
    }

    return (
        <>
            {finalized.map((md, fi) => (
                <FinalizedSegment
                    key={`fin-${segmentIndex}-${fi}`}
                    markdown={md}
                    components={components}
                />
            ))}
            {inProgress && (
                <ReactMarkdown
                    remarkPlugins={[remarkGfm]}
                    components={components}
                >
                    {inProgress}
                </ReactMarkdown>
            )}
        </>
    );
};

// Memoize: only re-render when content, streaming state, or editor info changes
export const MarkdownRenderer = React.memo(MarkdownRendererInner, (prev, next) => {
    return (
        prev.content === next.content &&
        prev.isStreaming === next.isStreaming &&
        prev.editorFilePath === next.editorFilePath &&
        prev.editorFileName === next.editorFileName
    );
});

export { ToolChip };
export type { ContentSegment };
