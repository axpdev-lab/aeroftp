// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type GuiErrorCode = 'unsupported_intent' | 'invalid_args' | 'locked' | 'busy' |
    'paused' | 'speed_locked' | 'blocked' | 'stale_state' | 'lease_interrupted' | 'gui_timeout' | 'action_failed' | 'not_connected' | 'pending_human';
export class GuiError extends Error {
    /** `detail` says what to change: a field and what it accepts. Never a value. */
    constructor(public readonly code: GuiErrorCode, public readonly detail?: string) { super(detail ? `${code}: ${detail}` : code); }
}
