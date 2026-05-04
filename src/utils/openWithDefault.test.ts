// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
  buildTerminalCommandForPath,
  getOpenWithDefaultRoute,
  isSafeLocalOpenPath,
  shellQuoteLocalPath,
} from './openWithDefault';

describe('openWithDefault routing', () => {
  it('routes AeroFTP-owned formats internally', () => {
    expect(getOpenWithDefaultRoute('/tmp/vault.aerovault').kind).toBe('aerovault');
    expect(getOpenWithDefaultRoute('/tmp/profiles.aeroftp').kind).toBe('aeroftp-profile');
    expect(getOpenWithDefaultRoute('/tmp/backup.aeroftp-keystore').kind).toBe('aeroftp-keystore');
  });

  it('routes executable scripts to terminal commands', () => {
    expect(getOpenWithDefaultRoute('/tmp/run me.sh', false, 'Linux')).toEqual({
      kind: 'terminal',
      command: "bash '/tmp/run me.sh'",
    });
    expect(getOpenWithDefaultRoute('/tmp/task.ps1', false, 'Linux')).toEqual({
      kind: 'terminal',
      command: "pwsh -File '/tmp/task.ps1'",
    });
  });

  it('falls back to system routing for normal files and directories', () => {
    expect(getOpenWithDefaultRoute('/tmp/report.pdf').kind).toBe('system');
    expect(getOpenWithDefaultRoute('/tmp/scripts', true).kind).toBe('system');
  });

  it('quotes shell paths without allowing injection through single quotes', () => {
    expect(shellQuoteLocalPath("/tmp/a'b.sh", 'Linux')).toBe("'/tmp/a'\\''b.sh'");
    expect(shellQuoteLocalPath("C:\\A B\\a'b.ps1", 'Win32')).toBe("'C:\\A B\\a''b.ps1'");
  });

  it('rejects URI and control-character paths', () => {
    expect(isSafeLocalOpenPath('file:///tmp/a.txt')).toBe(false);
    expect(isSafeLocalOpenPath('https://example.com/a.txt')).toBe(false);
    expect(isSafeLocalOpenPath('/tmp/a\nb.txt')).toBe(false);
    expect(buildTerminalCommandForPath('file:///tmp/a.sh')).toBeNull();
  });
});
