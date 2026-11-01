// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The run a file transfer belongs to. A sync or a selection of files can send
 * one transfer per file, each with its own id, and nothing in those transfers'
 * events ties them together. The progress card needs to tell the next file of
 * a run, which keeps the graph and the height on screen, from a separate
 * transfer that starts while the card is still closing, which does not.
 */
let current: string | null = null;
let open = 0;
let counter = 0;

/**
 * Run `work` as one transfer run. Work that overlaps a run under way joins it,
 * because the app has one progress card and both show on it; the run lasts
 * until the last of them ends, or the files still running would lose it.
 */
export async function withTransferRun<R>(work: () => R | Promise<R>): Promise<R> {
    if (open++ === 0) current = `run-${++counter}`;
    try {
        return await work();
    } finally {
        if (--open === 0) current = null;
    }
}

export const currentTransferRun = (): string | null => current;
