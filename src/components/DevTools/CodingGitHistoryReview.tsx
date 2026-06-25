// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { GitCommit, History } from 'lucide-react';
import type {
    CodingGitHistoryResultData,
    CodingGitLogResult,
    CodingGitShowResult,
} from './aiChatTypes';

interface CodingGitHistoryReviewProps {
    data: CodingGitHistoryResultData;
}

const chipClass = 'rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65';

const isLogResult = (
    data: CodingGitHistoryResultData,
): data is CodingGitHistoryResultData & { result: CodingGitLogResult } => (
    data.toolName === 'coding_git_log'
);

const LogCard: React.FC<{ result: CodingGitLogResult }> = ({ result }) => (
    <div className="mt-2 rounded-xl border border-white/10 bg-white/[0.03] p-3">
        <div className="flex items-center gap-2">
            <History className="h-4 w-4 text-white/55" />
            <span className="text-xs font-semibold text-white/85">Git Log</span>
            <span className={`ml-auto ${chipClass}`}>{result.commits.length} commit(s)</span>
        </div>
        {result.paths.length > 0 && (
            <div className="mt-1 truncate text-[10px] text-white/45" title={result.paths.join(', ')}>
                paths: {result.paths.join(', ')}
            </div>
        )}
        <div className="mt-2 space-y-1">
            {result.commits.map(commit => (
                <div key={commit.hash} className="flex items-center gap-2">
                    <span className="w-16 shrink-0 rounded border border-white/10 bg-black/20 px-1 py-0.5 text-center font-mono text-[10px] text-white/55">
                        {commit.short_hash}
                    </span>
                    <span className="min-w-0 flex-1 truncate text-[11px] text-white/80" title={commit.subject}>
                        {commit.subject}
                    </span>
                    <span className="shrink-0 text-[10px] text-white/40" title={commit.date}>
                        {commit.author}
                    </span>
                </div>
            ))}
        </div>
        {result.truncated && (
            <div className="mt-1 text-[10px] text-white/40">Showing newest {result.max_count}; more exist.</div>
        )}
    </div>
);

const ShowCard: React.FC<{ result: CodingGitShowResult }> = ({ result }) => (
    <div className="mt-2 rounded-xl border border-white/10 bg-white/[0.03] p-3">
        <div className="flex items-center gap-2">
            <GitCommit className="h-4 w-4 text-white/55" />
            <span className="font-mono text-xs font-semibold text-white/85">{result.short_hash}</span>
            <span className="ml-auto flex items-center gap-1.5">
                <span className={chipClass}>+{result.total_additions}</span>
                <span className={chipClass}>-{result.total_deletions}</span>
            </span>
        </div>
        <div className="mt-1 text-[12px] font-medium text-white/85">{result.subject}</div>
        <div className="mt-0.5 text-[10px] text-white/45">{result.author} · {result.date}</div>
        {result.body.trim() && (
            <pre className="mt-2 max-h-24 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-white/10 bg-black/20 p-2 font-mono text-[11px] text-white/65">
                {result.body}
            </pre>
        )}
        {result.stats.length > 0 && (
            <div className="mt-2 space-y-1">
                {result.stats.slice(0, 12).map(stat => (
                    <div key={stat.path} className="flex items-center gap-2">
                        <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-white/75" title={stat.path}>
                            {stat.path}
                        </span>
                        {stat.binary ? (
                            <span className="text-[10px] text-white/40">binary</span>
                        ) : (
                            <span className="shrink-0 font-mono text-[10px]">
                                <span className="text-emerald-300/80">+{stat.additions}</span>{' '}
                                <span className="text-red-300/80">-{stat.deletions}</span>
                            </span>
                        )}
                    </div>
                ))}
                {result.stats.length > 12 && (
                    <div className="text-[10px] text-white/40">+{result.stats.length - 12} more</div>
                )}
            </div>
        )}
        {result.diff.trim() && (
            <pre className="mt-2 max-h-56 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-white/10 bg-black/30 p-2 font-mono text-[11px] leading-snug text-white/75">
                {result.diff}
            </pre>
        )}
        {result.truncated && (
            <div className="mt-1 text-[10px] text-amber-300/70">Diff output was truncated.</div>
        )}
    </div>
);

export const CodingGitHistoryReview: React.FC<CodingGitHistoryReviewProps> = ({ data }) => {
    if (isLogResult(data)) {
        return <LogCard result={data.result} />;
    }
    return <ShowCard result={data.result as CodingGitShowResult} />;
};

export default CodingGitHistoryReview;
