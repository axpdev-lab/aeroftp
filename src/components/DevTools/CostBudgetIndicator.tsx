// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { DollarSign, AlertTriangle } from 'lucide-react';
import { ConversationCost, BudgetCheckResult } from './CostBudgetManager';
import { conversationCostView } from './aiChatCostEstimate';
import type { Message } from './aiChatTypes';
import { useTranslation } from '../../i18n';

interface CostBudgetIndicatorProps {
    conversationCost: ConversationCost | null;
    budgetCheck: BudgetCheckResult | null;
    /** The `showCostEstimates` setting: when off, token counts only. */
    showCostEstimates: boolean;
    /** A reply of this conversation whose cost was not estimated, if any. */
    withheldCost?: NonNullable<Message['tokenInfo']>;
}

export const CostBudgetIndicator: React.FC<CostBudgetIndicatorProps> = ({
    conversationCost,
    budgetCheck,
    showCostEstimates,
    withheldCost,
}) => {
    const t = useTranslation();
    if (!conversationCost && !budgetCheck) return null;

    const cost = conversationCost?.totalCost || 0;
    const tokens = conversationCost?.totalTokens || 0;
    const costView = conversationCostView(cost, withheldCost, { enabled: showCostEstimates, t });

    const isWarning = budgetCheck?.warning || false;
    const isBlocked = budgetCheck ? !budgetCheck.allowed : false;

    return (
        <div className={`flex items-center gap-1 text-[10px] ${
            isBlocked ? 'text-red-400' : isWarning ? 'text-yellow-400' : 'text-gray-500'
        }`}>
            {isWarning && <AlertTriangle size={9} />}
            {costView.kind !== 'hidden' && (
                <>
                    <DollarSign size={9} />
                    <span className="cursor-help" title={costView.title}>{costView.text}</span>
                    <span className="text-gray-600">|</span>
                </>
            )}
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
};

export default CostBudgetIndicator;
