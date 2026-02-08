import React, { useState, useEffect, useRef } from 'react';
import { Brain, ChevronDown, ChevronRight } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface ThinkingBlockProps {
    content: string;
    isComplete: boolean;
    duration?: number;
    thinkingTokens?: number;    // Tokens used for thinking
    responseTokens?: number;    // Tokens used for response
}

export const ThinkingBlock: React.FC<ThinkingBlockProps> = ({ content, isComplete, duration, thinkingTokens, responseTokens }) => {
    const [expanded, setExpanded] = useState(!isComplete);
    const [userToggled, setUserToggled] = useState(false);
    const [elapsed, setElapsed] = useState(duration || 0);
    const startRef = useRef(Date.now());
    const t = useTranslation();

    // Live timer while thinking is in progress
    useEffect(() => {
        if (isComplete) {
            if (duration) setElapsed(duration);
            // Only auto-collapse if the user hasn't manually toggled
            if (!userToggled) setExpanded(false);
            return;
        }
        startRef.current = Date.now();
        const interval = setInterval(() => {
            setElapsed(Math.round((Date.now() - startRef.current) / 1000));
        }, 1000);
        return () => clearInterval(interval);
    }, [isComplete, duration, userToggled]);

    const headerLabel = isComplete
        ? (t('ai.thinkingBlock.thoughtFor', { seconds: elapsed }) || `Thought for ${elapsed}s`)
        : `${t('ai.thinkingBlock.thinking') || 'Thinking...'} ${elapsed}s`;

    return (
        <div className="my-2 rounded border border-gray-700/50 bg-gray-900/40 overflow-hidden">
            {/* Header — clickable to expand/collapse */}
            <button
                onClick={() => { setExpanded(!expanded); setUserToggled(true); }}
                className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-gray-400 hover:text-gray-300 transition-colors"
            >
                <Brain size={13} className={isComplete ? 'text-purple-400' : 'text-purple-400 animate-pulse'} />
                <span className="font-medium">{headerLabel}</span>
                {isComplete && thinkingTokens && (
                    <span className="ml-auto text-[10px] text-gray-600 font-mono">
                        {thinkingTokens.toLocaleString()} tok
                        {responseTokens && (
                            <span className="text-gray-700"> / {(thinkingTokens + responseTokens).toLocaleString()}</span>
                        )}
                    </span>
                )}
                <span className={isComplete && thinkingTokens ? '' : 'ml-auto'}>
                    {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                </span>
            </button>

            {/* Content — collapsible */}
            {expanded && (
                <div className="px-3 pb-2 border-t border-gray-700/30">
                    <div className="text-[11px] text-gray-500 italic font-mono leading-relaxed whitespace-pre-wrap max-h-60 overflow-y-auto mt-1.5">
                        {content}
                    </div>
                </div>
            )}
        </div>
    );
};

export default ThinkingBlock;
