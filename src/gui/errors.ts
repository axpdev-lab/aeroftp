// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type GuiErrorCode = 'unsupported_intent' | 'invalid_args' | 'locked' | 'busy' |
    'blocked' | 'stale_state' | 'lease_interrupted' | 'gui_timeout' | 'action_failed' | 'not_connected' | 'pending_human';
export class GuiError extends Error {
    constructor(public readonly code: GuiErrorCode) { super(code); }
}
