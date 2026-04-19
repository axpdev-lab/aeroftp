// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import React, { useState } from 'react';
import { Loader2 } from 'lucide-react';
import { useTauriListener } from '../../hooks/useTauriListener';

interface ProgressEvent {
    tool: string;
    current: number;
    total: number;
    item: string;
}

interface ToolProgressIndicatorProps {
    toolName: string;
}

export const ToolProgressIndicator: React.FC<ToolProgressIndicatorProps> = ({ toolName }) => {
    const [progress, setProgress] = useState<ProgressEvent | null>(null);

    useTauriListener<ProgressEvent>('ai-tool-progress', (event) => {
        if (event.payload.tool === toolName) {
            setProgress(event.payload);
        }
    }, [toolName]);

    if (!progress) return null;

    const pct = progress.total > 0 ? Math.round((progress.current / progress.total) * 100) : 0;

    return (
        <div className="px-3 py-1 text-[11px] text-gray-400 flex items-center gap-2">
            <Loader2 size={10} className="animate-spin text-yellow-400" />
            <div className="flex-1">
                <div className="flex items-center justify-between mb-0.5">
                    <span className="truncate max-w-[180px]">{progress.item}</span>
                    <span className="text-gray-500">{progress.current}/{progress.total}</span>
                </div>
                <div className="w-full h-1 bg-gray-700 rounded-full overflow-hidden">
                    <div
                        className="h-full bg-yellow-500/70 rounded-full transition-all duration-300"
                        style={{ width: `${pct}%` }}
                    />
                </div>
            </div>
        </div>
    );
};

export default ToolProgressIndicator;
