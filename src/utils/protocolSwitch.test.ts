// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { scpFallbackAllowed } from './scpFallback';
import { optionsAcrossSshSwitch, presetFitsProtocol, storedPresetForSave } from './protocolSwitch';
import { getProviderById } from '../providers';

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

    it('keeps the quota settings of the server across the switch', () => {
        const quota = { timeout: 5, manualTotalBytes: 8_000_000_000, autoScanUsedOnConnect: true };
        expect(optionsAcrossSshSwitch(optionsAcrossSshSwitch(quota))).toEqual(quota);
    });

    it('drops a preset only when it speaks another protocol', () => {
        expect(presetFitsProtocol('sftp', 'scp')).toBe(false);
        expect(presetFitsProtocol('sftp', 'ftp')).toBe(false);
        expect(presetFitsProtocol('sftp', 'sftp')).toBe(true);
        expect(presetFitsProtocol(undefined, 'scp')).toBe(true);
    });
});

describe('saving an edited profile after a protocol switch', () => {
    const presetProtocolOf = (id: string) => getProviderById(id)?.protocol;

    it('does not take back a stored preset of another protocol', () => {
        // SourceForge is an SFTP preset: saved as SCP it would reopen as SFTP.
        expect(presetProtocolOf('sourceforge')).toBe('sftp');
        expect(storedPresetForSave('sourceforge', 'scp', presetProtocolOf)).toBeUndefined();
        expect(storedPresetForSave('sourceforge', 'ftp', presetProtocolOf)).toBeUndefined();
        expect(storedPresetForSave('sourceforge', 'sftp', presetProtocolOf)).toBe('sourceforge');
    });

    it('keeps a stored id outside the switch, or one the registry does not know', () => {
        // A native Koofr profile stores the id of the Koofr WebDAV preset.
        expect(presetProtocolOf('koofr')).toBe('webdav');
        expect(storedPresetForSave('koofr', 'koofr', presetProtocolOf)).toBe('koofr');
        expect(storedPresetForSave('mtp-portable', 'mtp', presetProtocolOf)).toBe('mtp-portable');
        expect(storedPresetForSave('not-in-registry', 'scp', presetProtocolOf)).toBe('not-in-registry');
        expect(storedPresetForSave(undefined, 'scp', presetProtocolOf)).toBeUndefined();
    });
});
