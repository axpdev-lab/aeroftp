import React from 'react';
import { DollarSign, AlertTriangle } from 'lucide-react';
import { ConversationCost, formatCost, BudgetCheckResult } from './CostBudgetManager';

interface CostBudgetIndicatorProps {
    conversationCost: ConversationCost | null;
    budgetCheck: BudgetCheckResult | null;
    compact?: boolean;
}

export const CostBudgetIndicator: React.FC<CostBudgetIndicatorProps> = ({
    conversationCost,
    budgetCheck,
    compact = true,
}) => {
    if (!conversationCost && !budgetCheck) return null;

    const cost = conversationCost?.totalCost || 0;
    const tokens = conversationCost?.totalTokens || 0;
    const requests = conversationCost?.requestCount || 0;

    const isWarning = budgetCheck?.warning || false;
    const isBlocked = budgetCheck ? !budgetCheck.allowed : false;

    if (compact) {
        return (
            <div className={`flex items-center gap-1 text-[10px] ${
                isBlocked ? 'text-red-400' : isWarning ? 'text-yellow-400' : 'text-gray-500'
            }`}>
                {isWarning && <AlertTriangle size={9} />}
                <DollarSign size={9} />
                <span>{formatCost(cost)}</span>
                <span className="text-gray-600">|</span>
                <span>{tokens.toLocaleString()} tok</span>
                {budgetCheck && budgetCheck.limit > 0 && (
                    <>
                        <span className="text-gray-600">|</span>
                        <span className={isWarning ? 'text-yellow-400' : ''}>
                            {budgetCheck.percentUsed}%
                        </span>
                    </>
                )}
            </div>
        );
    }

    // Expanded view (for settings or tooltip)
    return (
        <div className="p-2 bg-gray-800/50 rounded border border-gray-700/50 text-xs space-y-1">
            <div className="flex items-center justify-between">
                <span className="text-gray-400">Conversation cost</span>
                <span className={`font-mono ${isBlocked ? 'text-red-400' : 'text-green-400'}`}>
                    {formatCost(cost)}
                </span>
            </div>
            <div className="flex items-center justify-between">
                <span className="text-gray-400">Tokens</span>
                <span className="font-mono text-gray-300">{tokens.toLocaleString()}</span>
            </div>
            <div className="flex items-center justify-between">
                <span className="text-gray-400">Requests</span>
                <span className="font-mono text-gray-300">{requests}</span>
            </div>
            {budgetCheck && budgetCheck.limit > 0 && (
                <>
                    <div className="border-t border-gray-700/50 pt-1 mt-1">
                        <div className="flex items-center justify-between">
                            <span className="text-gray-400">Monthly budget</span>
                            <span className="font-mono text-gray-300">
                                {formatCost(budgetCheck.currentSpend)} / {formatCost(budgetCheck.limit)}
                            </span>
                        </div>
                        {/* Progress bar */}
                        <div className="h-1 bg-gray-700 rounded-full mt-1 overflow-hidden">
                            <div
                                className={`h-full rounded-full transition-all ${
                                    isBlocked ? 'bg-red-500' : isWarning ? 'bg-yellow-500' : 'bg-green-500'
                                }`}
                                style={{ width: `${Math.min(100, budgetCheck.percentUsed)}%` }}
                            />
                        </div>
                    </div>
                    {budgetCheck.message && (
                        <div className={`text-[10px] ${isBlocked ? 'text-red-400' : 'text-yellow-400'}`}>
                            {budgetCheck.message}
                        </div>
                    )}
                </>
            )}
        </div>
    );
};

export default CostBudgetIndicator;
