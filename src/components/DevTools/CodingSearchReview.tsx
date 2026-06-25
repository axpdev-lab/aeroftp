// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React from 'react';
import { Search, TimerReset } from 'lucide-react';
import { searchMatchLocation } from './aiChatCodingSearch';
import type { CodingSearchMatch, CodingSearchResultData } from './aiChatTypes';

interface CodingSearchReviewProps {
    data: CodingSearchResultData;
}

const chipClass = 'rounded-full border border-white/10 bg-white/5 px-2 py-0.5 text-[10px] text-white/65';

const MAX_VISIBLE = 50;

const MatchRow: React.FC<{ match: CodingSearchMatch }> = ({ match }) => {
    const location = searchMatchLocation(match);
    return (
        <div className="rounded-lg border border-white/10 bg-white/5 p-2">
            <div
                className="truncate font-mono text-[10px] text-white/55"
                title={location}
            >
                {location}
            </div>
            {match.line_text && (
                <div className="mt-1 overflow-x-auto whitespace-pre font-mono text-[11px] text-white/80">
                    {match.line_text}
                </div>
            )}
        </div>
    );
};

export const CodingSearchReview: React.FC<CodingSearchReviewProps> = ({ data }) => {
    const { result } = data;
    const tone = result.timed_out ? 'text-amber-200' : 'text-emerald-200';
    const StateIcon = result.timed_out ? TimerReset : Search;
    const stateLabel = result.timed_out
        ? 'timed-out'
        : `${result.total_matches} match(es)`;
    const visible = result.matches.slice(0, MAX_VISIBLE);
    const hidden = result.matches.length - visible.length;

    return (
        <div className="mt-2 rounded-xl border border-white/10 bg-white/[0.03] p-3">
            <div className="flex items-center gap-2">
                <Search className="h-4 w-4 text-white/55" />
                <span className="text-xs font-semibold text-white/85">Workspace Search</span>
                <span className={`ml-auto flex items-center gap-1 text-[11px] font-semibold ${tone}`}>
                    <StateIcon className="h-3.5 w-3.5" />
                    {stateLabel}
                </span>
            </div>

            <div className="mt-2 flex flex-wrap items-center gap-1.5">
                <span className={`${chipClass} font-mono`} title={result.pattern}>{result.pattern}</span>
                {result.fixed_strings && <span className={chipClass}>literal</span>}
                {result.case_insensitive && <span className={chipClass}>ignore-case</span>}
                {result.path && <span className={chipClass}>in {result.path}</span>}
                <span className={chipClass}>{result.file_count} file(s)</span>
                {result.timed_out ? (
                    <span className={chipClass}>timeout {result.timeout_secs}s</span>
                ) : (
                    <span className={chipClass}>{(result.duration_ms / 1000).toFixed(1)}s</span>
                )}
            </div>

            {result.matches.length > 0 ? (
                <div className="mt-2 space-y-2">
                    {visible.map((match, idx) => (
                        <MatchRow key={`${match.file}-${match.line}-${match.column}-${idx}`} match={match} />
                    ))}
                    {hidden > 0 && (
                        <div className="text-[10px] text-white/40">+{hidden} more</div>
                    )}
                    {result.truncated && (
                        <div className="text-[10px] text-amber-300/70">Results were truncated.</div>
                    )}
                </div>
            ) : (
                <div className="mt-2 text-[11px] text-white/55">
                    {result.timed_out ? `Timed out after ${result.timeout_secs}s.` : 'No matches found.'}
                </div>
            )}
        </div>
    );
};

export default CodingSearchReview;
