import React, { useState } from 'react';
import { Play, ChevronDown, ChevronRight, Copy, Check } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface GeminiCodeBlockProps {
    code: string;
    language: string;
    output: string;
    outcome: string; // 'OUTCOME_OK' | 'OUTCOME_FAILED' | etc.
}

export const GeminiCodeBlock: React.FC<GeminiCodeBlockProps> = ({ code, language, output, outcome }) => {
    const t = useTranslation();
    const [expanded, setExpanded] = useState(true);
    const [copied, setCopied] = useState(false);

    const isSuccess = outcome === 'OUTCOME_OK';

    const handleCopy = async () => {
        await navigator.clipboard.writeText(code);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    };

    return (
        <div className="my-2 rounded-lg border border-gray-700 overflow-hidden">
            {/* Header */}
            <div className="flex items-center justify-between px-3 py-1.5 bg-gray-800/50 border-b border-gray-700">
                <div className="flex items-center gap-2 text-xs text-gray-400">
                    <Play size={12} className="text-green-400" />
                    <span>Gemini Code Execution</span>
                    <span className="text-gray-600">({language.toLowerCase()})</span>
                </div>
                <button onClick={handleCopy} className="text-gray-500 hover:text-gray-300 transition-colors" title={t('common.copy')}>
                    {copied ? <Check size={12} className="text-green-400" /> : <Copy size={12} />}
                </button>
            </div>

            {/* Code */}
            <pre className="px-3 py-2 text-xs text-gray-300 bg-gray-900/50 overflow-x-auto">
                <code>{code}</code>
            </pre>

            {/* Output section */}
            {output && (
                <div className="border-t border-gray-700">
                    <button
                        onClick={() => setExpanded(!expanded)}
                        className="flex items-center gap-1.5 w-full px-3 py-1.5 text-xs text-gray-400 hover:text-gray-300 bg-gray-800/30"
                    >
                        {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                        <span>Output</span>
                        <span className={`ml-auto text-[10px] px-1.5 py-0.5 rounded ${isSuccess ? 'bg-green-900/30 text-green-400' : 'bg-red-900/30 text-red-400'}`}>
                            {isSuccess ? 'OK' : outcome.replace('OUTCOME_', '')}
                        </span>
                    </button>
                    {expanded && (
                        <pre className="px-3 py-2 text-xs text-gray-300 bg-gray-950/50 overflow-x-auto max-h-48 overflow-y-auto">
                            <code>{output}</code>
                        </pre>
                    )}
                </div>
            )}
        </div>
    );
};

/**
 * Parse message content for Gemini code execution blocks.
 * Returns an array of segments: either plain text or code execution blocks.
 */
export interface CodeExecSegment {
    type: 'text' | 'code_exec';
    content: string;
    code?: string;
    language?: string;
    output?: string;
    outcome?: string;
}

export function parseGeminiCodeExec(content: string): CodeExecSegment[] {
    // Look for the pattern our Rust backend produces:
    // ```python\n{code}\n```\n\n**Execution Output** ({outcome}):\n```\n{output}\n```
    const regex = /```(\w+)\n([\s\S]*?)```\n\n\*\*Execution Output\*\* \((\w+)\):\n```\n([\s\S]*?)```/g;

    const segments: CodeExecSegment[] = [];
    let lastIndex = 0;
    let match;

    while ((match = regex.exec(content)) !== null) {
        // Add text before match
        if (match.index > lastIndex) {
            segments.push({ type: 'text', content: content.slice(lastIndex, match.index) });
        }

        segments.push({
            type: 'code_exec',
            content: match[0],
            language: match[1],
            code: match[2].trim(),
            outcome: match[3],
            output: match[4].trim(),
        });

        lastIndex = match.index + match[0].length;
    }

    // Add remaining text
    if (lastIndex < content.length) {
        segments.push({ type: 'text', content: content.slice(lastIndex) });
    }

    // If no matches found, return single text segment
    if (segments.length === 0) {
        segments.push({ type: 'text', content });
    }

    return segments;
}
