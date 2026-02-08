import React, { useMemo } from 'react';
import { AlertTriangle } from 'lucide-react';
import { useTranslation } from '../../i18n';

export interface TokenBudgetData {
    modelMaxTokens: number;
    systemPromptTokens: number;
    contextTokens: number;     // Smart context (#66, #67, #68, #70)
    historyTokens: number;
    currentMessageTokens: number;
    responseBuffer: number;
}

interface TokenBudgetIndicatorProps {
    budget: TokenBudgetData | null;
    compact?: boolean;
}

function formatTokenCount(tokens: number): string {
    if (tokens >= 1000) return `${(tokens / 1000).toFixed(1)}K`;
    return String(tokens);
}

/** Compact token budget bar for the chat input area */
export const TokenBudgetIndicator: React.FC<TokenBudgetIndicatorProps> = ({ budget, compact = false }) => {
    const t = useTranslation();

    const breakdown = useMemo(() => {
        if (!budget) return null;
        const total = budget.modelMaxTokens;
        if (total <= 0) return null;

        const used = budget.systemPromptTokens + budget.contextTokens +
                     budget.historyTokens + budget.currentMessageTokens + budget.responseBuffer;
        const available = Math.max(0, total - used);
        const usagePercent = Math.min(100, Math.round((used / total) * 100));

        return {
            total,
            used,
            available,
            usagePercent,
            segments: [
                { label: t('ai.budget.system') || 'System', tokens: budget.systemPromptTokens, color: 'bg-blue-500' },
                { label: t('ai.budget.context') || 'Context', tokens: budget.contextTokens, color: 'bg-emerald-500' },
                { label: t('ai.budget.history') || 'History', tokens: budget.historyTokens, color: 'bg-yellow-500' },
                { label: t('ai.budget.current') || 'Current', tokens: budget.currentMessageTokens, color: 'bg-purple-500' },
                { label: t('ai.budget.response') || 'Response', tokens: budget.responseBuffer, color: 'bg-gray-600' },
            ].filter(s => s.tokens > 0),
        };
    }, [budget, t]);

    if (!breakdown) return null;

    const isWarning = breakdown.usagePercent >= 80;
    const isCritical = breakdown.usagePercent >= 95;

    if (compact) {
        return (
            <div className="flex items-center gap-1.5 px-3 py-1 text-[10px] text-gray-500">
                {isWarning && <AlertTriangle size={10} className={isCritical ? 'text-red-400' : 'text-yellow-400'} />}
                <div className="w-20 h-1.5 bg-gray-700 rounded-full overflow-hidden flex">
                    {breakdown.segments.map((seg, i) => (
                        <div
                            key={i}
                            className={`${seg.color} h-full`}
                            style={{ width: `${(seg.tokens / breakdown.total) * 100}%` }}
                        />
                    ))}
                </div>
                <span className={isCritical ? 'text-red-400' : isWarning ? 'text-yellow-400' : ''}>
                    {formatTokenCount(breakdown.used)}/{formatTokenCount(breakdown.total)}
                </span>
            </div>
        );
    }

    return (
        <div className="px-3 py-1.5 border-t border-gray-700/50 bg-gray-800/30">
            {/* Token bar */}
            <div className="flex items-center gap-2 mb-1">
                {isWarning && <AlertTriangle size={11} className={isCritical ? 'text-red-400' : 'text-yellow-400'} />}
                <div className="flex-1 h-2 bg-gray-700 rounded-full overflow-hidden flex">
                    {breakdown.segments.map((seg, i) => (
                        <div
                            key={i}
                            className={`${seg.color} h-full transition-all duration-300`}
                            style={{ width: `${(seg.tokens / breakdown.total) * 100}%` }}
                            title={`${seg.label}: ${formatTokenCount(seg.tokens)}`}
                        />
                    ))}
                </div>
                <span className={`text-[10px] font-mono tabular-nums ${
                    isCritical ? 'text-red-400' : isWarning ? 'text-yellow-400' : 'text-gray-500'
                }`}>
                    {breakdown.usagePercent}%
                </span>
            </div>

            {/* Legend */}
            <div className="flex flex-wrap gap-x-3 gap-y-0.5">
                {breakdown.segments.map((seg, i) => (
                    <div key={i} className="flex items-center gap-1 text-[10px] text-gray-500">
                        <div className={`w-1.5 h-1.5 rounded-full ${seg.color}`} />
                        <span>{seg.label}</span>
                        <span className="font-mono tabular-nums">{formatTokenCount(seg.tokens)}</span>
                    </div>
                ))}
                <div className="flex items-center gap-1 text-[10px] text-gray-500">
                    <div className="w-1.5 h-1.5 rounded-full bg-gray-800 border border-gray-600" />
                    <span>{t('ai.budget.available') || 'Available'}</span>
                    <span className="font-mono tabular-nums">{formatTokenCount(breakdown.available)}</span>
                </div>
            </div>
        </div>
    );
};

export default TokenBudgetIndicator;
