// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { archiveStem, inferGeneralKind, isWrongPasswordError, needsPasswordPrompt } from './extractOrchestrator';

describe('archiveStem (Extract-to-folder destination derivation)', () => {
    it('strips single archive extensions', () => {
        expect(archiveStem('photos.zip')).toBe('photos');
        expect(archiveStem('backup.7z')).toBe('backup');
        expect(archiveStem('data.rar')).toBe('data');
        expect(archiveStem('logs.tar')).toBe('logs');
        expect(archiveStem('vault.aerovault')).toBe('vault');
        expect(archiveStem('bundle.aerozip')).toBe('bundle');
    });

    it('strips multi-part tar extensions', () => {
        expect(archiveStem('logs.tar.gz')).toBe('logs');
        expect(archiveStem('logs.tar.xz')).toBe('logs');
        expect(archiveStem('logs.tar.bz2')).toBe('logs');
        expect(archiveStem('logs.tgz')).toBe('logs');
    });

    it('is case-insensitive on the extension', () => {
        expect(archiveStem('PHOTOS.ZIP')).toBe('PHOTOS');
        expect(archiveStem('Logs.TAR.GZ')).toBe('Logs');
    });

    it('keeps internal dots and unknown/absent extensions', () => {
        expect(archiveStem('release.1.2.3.zip')).toBe('release.1.2.3');
        expect(archiveStem('noext')).toBe('noext');
        expect(archiveStem('.bashrc')).toBe('.bashrc');
    });

    it('matches the Rust archive_extract_stem behavior it mirrors', () => {
        // Same fixtures asserted in src-tauri extract_intent_tests, kept in lockstep.
        expect(archiveStem('photos.zip')).toBe('photos');
        expect(archiveStem('logs.tar.bz2')).toBe('logs');
    });
});

describe('inferGeneralKind (general archive routing)', () => {
    it('maps each general format', () => {
        expect(inferGeneralKind('a.zip')).toBe('zip');
        expect(inferGeneralKind('a.7z')).toBe('sevenz');
        expect(inferGeneralKind('a.rar')).toBe('rar');
        expect(inferGeneralKind('a.tar')).toBe('tar');
        expect(inferGeneralKind('a.tar.gz')).toBe('tar');
        expect(inferGeneralKind('a.tgz')).toBe('tar');
        expect(inferGeneralKind('a.tar.xz')).toBe('tar');
        expect(inferGeneralKind('a.tbz2')).toBe('tar');
    });

    it('maps standalone single-stream codecs to single', () => {
        expect(inferGeneralKind('a.gz')).toBe('single');
        expect(inferGeneralKind('a.xz')).toBe('single');
        expect(inferGeneralKind('a.bz2')).toBe('single');
        expect(inferGeneralKind('report.txt.gz')).toBe('single');
    });

    it('keeps tar.* on the tar lane, not single (checked before gz/xz/bz2)', () => {
        expect(inferGeneralKind('a.tar.gz')).toBe('tar');
        expect(inferGeneralKind('a.tar.bz2')).toBe('tar');
        expect(inferGeneralKind('a.tar.xz')).toBe('tar');
    });

    it('returns null for non-general (aero / unknown) names', () => {
        expect(inferGeneralKind('a.aerovault')).toBeNull();
        expect(inferGeneralKind('a.aerozip')).toBeNull();
        expect(inferGeneralKind('a.txt')).toBeNull();
        expect(inferGeneralKind('a')).toBeNull();
    });
});

describe('needsPasswordPrompt (encryption routing)', () => {
    it('clear archive extracts with no prompt', () => {
        expect(needsPasswordPrompt({ encrypted: false })).toBe(false);
    });

    it('encrypted archive prompts for a password', () => {
        expect(needsPasswordPrompt({ encrypted: true })).toBe(true);
    });
});

describe('isWrongPasswordError (extract error routing)', () => {
    it('classifies decryption-failure strings as wrong password', () => {
        expect(isWrongPasswordError('Invalid password or corrupt archive: bad MAC')).toBe(true);
        expect(isWrongPasswordError('Decryption failed: aead error')).toBe(true);
        expect(isWrongPasswordError('wrong password or tampered crypt config')).toBe(true);
        expect(isWrongPasswordError(new Error('Wrong secret'))).toBe(true);
    });

    it('does NOT misclassify real failures as wrong password', () => {
        expect(isWrongPasswordError('No space left on device')).toBe(false);
        expect(isWrongPasswordError('Permission denied (os error 13)')).toBe(false);
        expect(isWrongPasswordError('Read-only file system')).toBe(false);
        expect(isWrongPasswordError(new Error('destination is not writable'))).toBe(false);
        expect(isWrongPasswordError('')).toBe(false);
        expect(isWrongPasswordError(null)).toBe(false);
    });
});
