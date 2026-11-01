// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { scpFallbackAllowed } from './scpFallback';
import { optionsAcrossSshSwitch, presetFitsProtocol } from './protocolSwitch';

describe('protocol switch of a saved profile', () => {
    it('keeps an SFTP-only choice across SFTP to SCP and back', () => {
        const sftpOnly = { private_key_path: '/k', key_passphrase: 'p', timeout: 9, allowScpFallback: false, compression: true } as const;
        const toScp = optionsAcrossSshSwitch(sftpOnly);
        const back = optionsAcrossSshSwitch(toScp);
        expect(scpFallbackAllowed(back)).toBe(false);
        expect(back).toEqual({ private_key_path: '/k', key_passphrase: 'p', timeout: 9, allowScpFallback: false });
        // A profile that allows the fallback gains no key on the way.
        expect('allowScpFallback' in optionsAcrossSshSwitch({ timeout: 5 })).toBe(false);
        expect(scpFallbackAllowed(optionsAcrossSshSwitch(optionsAcrossSshSwitch({ timeout: 5 })))).toBe(true);
    });

    it('drops a preset only when it speaks another protocol', () => {
        expect(presetFitsProtocol('sftp', 'scp')).toBe(false);
        expect(presetFitsProtocol('sftp', 'ftp')).toBe(false);
        expect(presetFitsProtocol('sftp', 'sftp')).toBe(true);
        expect(presetFitsProtocol(undefined, 'scp')).toBe(true);
    });
});
