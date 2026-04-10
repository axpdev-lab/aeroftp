// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Eye, Loader2 } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface TransferActionBarProps {
    onPlan: () => void;
    canPlan: boolean;
    loading: boolean;
}

export const TransferActionBar: React.FC<TransferActionBarProps> = ({
    onPlan,
    canPlan,
    loading,
}) => {
    const t = useTranslation();

    return (
        <button
            onClick={onPlan}
            disabled={!canPlan || loading}
            className="w-full flex items-center justify-center gap-2 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed"
        >
            {loading ? <Loader2 className="w-4 h-4 animate-spin" /> : <Eye className="w-4 h-4" />}
            {t('transfer.crossProfile.previewPlan')}
        </button>
    );
};
