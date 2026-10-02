// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const commands = readFileSync('src-tauri/src/provider_commands.rs', 'utf8');
const segmented = commands.slice(
  commands.indexOf('let mut segmented_result:'),
  commands.indexOf('// If segmented ran'),
);

describe('GUI segmented download cancellation wiring', () => {
  it('inherits the live session cancellation without sharing engine fail-fast cancellation', () => {
    expect(segmented).toContain("let session_cancel = state.current_cancel_token().await;");
    expect(segmented).toContain("let cancel = session_cancel.child_token();");
    expect(segmented).not.toContain('CancellationToken::new()');
  });

  it('returns a cancellation event and error before the legacy fallback', () => {
    const errorBranch = segmented.slice(segmented.indexOf('if let Err(ref e) = outcome'));
    expect(errorBranch).toContain("session_cancel.is_cancelled()");
    expect(errorBranch).toContain("message_names_a_cancellation(e)");
    const cancellationBranch = errorBranch.slice(0, errorBranch.indexOf('warn!('));
    expect(cancellationBranch).toContain('emit_gui_transfer_event');
    expect(cancellationBranch).toContain('event_type: "error"');
    expect(cancellationBranch).toContain('return Err(');
  });
});
