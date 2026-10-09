// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { afterEach, describe, expect, it, vi } from 'vitest';
import { carriesSecret, maskedEcho, mayChangeFiles, replyText, siteCommandAvailable, stripAnsi, withoutReplyCode, type SiteCommandReport } from './siteCommand';
import { clearSiteTranscript, forgetSiteSession, recordSiteExchange, siteHistory, siteTranscript } from './siteSessionMemory';

const report = (over: Partial<SiteCommandReport> = {}): SiteCommandReport => ({
    outcome: 'replied', command: 'SITE WHO', verb_kind: 'read_only', code: 200, lines: ['200 ok'],
    encoding: 'utf8', elapsed_ms: 3, session_reset: false, session_reconnected: null, reason: null, ...over,
});

describe('SITE command helpers', () => {
    it('masks the password positions of known verbs in the echo only', () => {
        expect(maskedEcho('CHPASS alice hunter2')).toBe('SITE CHPASS alice ••••••');
        expect(maskedEcho('site adduser bob s3cret *@192.0.2.*')).toBe('SITE adduser bob •••••• *@192.0.2.*');
        expect(maskedEcho('GADDUSER staff carol pw')).toBe('SITE GADDUSER staff carol ••••••');
        expect(maskedEcho('USER alice')).toBe('SITE USER alice');
        expect(carriesSecret('SITE CHPASS a b')).toBe(true);
        expect(carriesSecret('CHANGE a ratio 5')).toBe(false);
    });

    it('strips reply codes and colours for reading, never the text', () => {
        expect(withoutReplyCode('200- | Username: x |')).toBe(' | Username: x |');
        expect(withoutReplyCode('200 Command Successful.')).toBe('Command Successful.');
        expect(withoutReplyCode('200-')).toBe('');
        expect(withoutReplyCode(' free text')).toBe(' free text');
        expect(stripAnsi('\u001b[1;31m200\u001b[0m- red')).toBe('200- red');
        expect(replyText(['200- a', '200 b'], true)).toBe(' a\nb');
        expect(replyText(['200- a', '200 b'], false)).toBe('200- a\n200 b');
        expect(replyText(['\u001b[1;31m200\u001b[0m- red'], true)).toBe(' red');
    });

    it('refreshes the listing unless the verb only reads or nothing was sent', () => {
        expect(mayChangeFiles(report())).toBe(false);
        expect(mayChangeFiles(report({ verb_kind: 'other' }))).toBe(true);
        expect(mayChangeFiles(report({ verb_kind: 'other', outcome: 'unknown', code: null }))).toBe(true);
        expect(mayChangeFiles(report({ verb_kind: 'other', outcome: 'not_sent', code: null }))).toBe(false);
    });
});

describe('SITE command availability', () => {
    it('is on only for a connected FTP or FTPS session', () => {
        expect(siteCommandAvailable(true, 'ftp')).toBe(true);
        expect(siteCommandAvailable(true, 'ftps')).toBe(true);
        expect(siteCommandAvailable(false, 'ftp')).toBe(false);
        for (const other of ['sftp', 'webdav', 's3', 'googledrive', 'mega', undefined]) {
            expect(siteCommandAvailable(true, other)).toBe(false);
        }
    });

    it('follows the active session, not the Quick Connect form', () => {
        expect(siteCommandAvailable(true, 'sftp', 'ftp')).toBe(false);
        expect(siteCommandAvailable(true, 'ftps', 'webdav')).toBe(true);
        expect(siteCommandAvailable(true, undefined, 'ftp')).toBe(true);
    });
});

describe('SITE session memory', () => {
    afterEach(() => {
        forgetSiteSession('s1');
        vi.unstubAllGlobals();
    });

    it('keeps history in memory, never in browser storage, and never a password line', () => {
        // The test environment is node: stand-in storages record any write.
        const setItem = vi.fn();
        vi.stubGlobal('localStorage', { setItem, getItem: vi.fn(), removeItem: vi.fn() });
        vi.stubGlobal('sessionStorage', { setItem, getItem: vi.fn(), removeItem: vi.fn() });
        recordSiteExchange('s1', 'WHO', maskedEcho('WHO'), report());
        recordSiteExchange('s1', 'CHPASS alice hunter2', maskedEcho('CHPASS alice hunter2'), report({ command: 'SITE CHPASS', verb_kind: 'other' }));
        recordSiteExchange('s1', 'WHO', maskedEcho('WHO'), report());
        expect(siteHistory('s1')).toEqual(['WHO']);
        expect(siteTranscript('s1').map(entry => entry.echo)).toEqual(['SITE WHO', 'SITE CHPASS alice ••••••', 'SITE WHO']);
        expect(JSON.stringify(siteTranscript('s1'))).not.toContain('hunter2');
        expect(setItem).not.toHaveBeenCalled();
    });

    it('forgets a session on disconnect and clears a transcript on request', () => {
        recordSiteExchange('s1', 'WHO', 'SITE WHO', report());
        clearSiteTranscript('s1');
        expect(siteTranscript('s1')).toEqual([]);
        expect(siteHistory('s1')).toEqual(['WHO']);
        forgetSiteSession('s1');
        expect(siteHistory('s1')).toEqual([]);
    });
});
