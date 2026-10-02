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
    expect(errorBranch).toContain("e.is_cancelled()");
    expect(errorBranch).not.toContain("message_names_a_cancellation");
    const cancellationBranch = errorBranch.slice(0, errorBranch.indexOf('warn!('));
    expect(cancellationBranch).toContain('emit_gui_transfer_event');
    expect(cancellationBranch).toContain('event_type: "error"');
    expect(cancellationBranch).toContain('return Err(');
  });
});

const executor = readFileSync('src-tauri/src/provider_transfer_executor.rs', 'utf8');
const crossProfile = readFileSync('src-tauri/src/cross_profile_transfer.rs', 'utf8');

describe('segmented publication boundaries', () => {
  it('reports success once the final file has been atomically committed', () => {
    const publication = executor.slice(
      executor.indexOf('None => match tokio::fs::rename(&temp, local_path).await'),
      executor.indexOf('Ok(ConcurrentRangeOutcome::ServerIgnoredRange) =>'),
    );
    expect(publication).toContain('Ok(()) => Ok(())');
    expect(publication).not.toContain('Ok(()) if cancel_token.is_cancelled()');
  });

  it('checks cancellation after staging succeeds before permitting a cross-profile upload', () => {
    const staging = crossProfile.slice(crossProfile.indexOf('match crate::provider_transfer_executor::run_provider_segmented_download('));
    const successReturn = staging.indexOf('Ok(()) => return Ok(())');
    const cancellationGuard = staging.indexOf('Ok(()) if options.cancel_token.is_cancelled()');
    expect(cancellationGuard).toBeGreaterThanOrEqual(0);
    expect(cancellationGuard).toBeLessThan(successReturn);
    expect(staging.slice(cancellationGuard, successReturn)).toContain('return Err(');
  });
});

it('rechecks Stop before destination directory, delta transfer, and classic upload', () => {
  const copy = crossProfile.slice(
    crossProfile.indexOf('pub async fn copy_one_file_with_options('),
    crossProfile.indexOf('async fn download_source_to_temp('),
  );
  let previousOperation = copy.indexOf('download_source_to_temp(');
  for (const operation of ['ensure_parent_dir(', 'try_delta_transfer(', 'dest.upload(']) {
    const position = copy.indexOf(operation);
    const guard = copy.lastIndexOf('check_copy_cancel(&options.cancel_token)?;', position);
    expect(guard).toBeGreaterThan(previousOperation);
    expect(guard).toBeLessThan(position);
    previousOperation = position;
  }
});

it('rechecks Stop immediately before the cross-profile single-stream fallback', () => {
  const staging = crossProfile.slice(
    crossProfile.indexOf('async fn download_source_to_temp('),
    crossProfile.indexOf('// ── Planning:'),
  );
  expect(staging).toContain(
    'check_copy_cancel(&options.cancel_token)?;\n    source.download(source_path, tmp_path, None).await',
  );
});
