// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { SiteCommandReport } from './siteCommand';
import { carriesSecret } from './siteCommand';

/** One exchange in the dialog transcript. */
export interface SiteTranscriptEntry {
    id: number;
    /** The echo shown in the transcript, passwords masked. */
    echo: string;
    report: SiteCommandReport;
}

interface SiteSessionMemory {
    transcript: SiteTranscriptEntry[];
    /** Typed lines for ArrowUp/ArrowDown, newest last. */
    history: string[];
}

const MAX_HISTORY = 100;
const MAX_TRANSCRIPT = 200;

/**
 * What the SITE dialog remembers, per session, in memory only: nothing here
 * reaches localStorage, the vault or a file, and a session's memory is
 * dropped when it disconnects. A line that carries a password (ADDUSER,
 * CHPASS...) never enters the history.
 */
const memory = new Map<string, SiteSessionMemory>();
let nextId = 1;

function memoryOf(sessionId: string): SiteSessionMemory {
    let entry = memory.get(sessionId);
    if (!entry) {
        entry = { transcript: [], history: [] };
        memory.set(sessionId, entry);
    }
    return entry;
}

export function siteTranscript(sessionId: string): SiteTranscriptEntry[] {
    return memory.get(sessionId)?.transcript ?? [];
}

export function siteHistory(sessionId: string): string[] {
    return memory.get(sessionId)?.history ?? [];
}

export function recordSiteExchange(sessionId: string, typed: string, echo: string, report: SiteCommandReport): SiteTranscriptEntry[] {
    const session = memoryOf(sessionId);
    session.transcript = [...session.transcript, { id: nextId++, echo, report }].slice(-MAX_TRANSCRIPT);
    if (!carriesSecret(typed)) {
        session.history = [...session.history.filter(line => line !== typed), typed].slice(-MAX_HISTORY);
    }
    return session.transcript;
}

export function clearSiteTranscript(sessionId: string): void {
    const session = memory.get(sessionId);
    if (session) session.transcript = [];
}

/** Drop everything remembered for a session (call on disconnect). */
export function forgetSiteSession(sessionId: string): void {
    memory.delete(sessionId);
}
