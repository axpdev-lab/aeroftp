// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Whether a finished tool may append its result or its error to the chat.
 *
 * `executePipeline` keeps awaiting the call that was already running when
 * Stop was pressed, then starts the next plan step. The chat may already
 * have been cleared or replaced. A call that names a turn publishes only
 * while that turn is still the active one. A call with no turn scope is
 * unchanged.
 */
export function toolOutputBelongsToActiveTurn(
    turnScope: string | undefined,
    activeTurn: string | null,
): boolean {
    return turnScope === undefined || activeTurn === turnScope;
}
