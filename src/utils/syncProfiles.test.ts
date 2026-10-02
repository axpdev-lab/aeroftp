// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import dialogSource from '../components/Sync/SyncTemplateDialog.tsx?raw';
import type { SyncProfile } from '../types';
import { deleteSavedSyncProfile, isDeletableSyncProfile } from './syncProfiles';

const profile = (id: string, builtin: boolean) => ({ id, name: id, builtin }) as SyncProfile;

/**
 * Applying an imported AeroSync script saves it as a sync preset, a second
 * import of the same script saves another `-imported-N` copy, and the saved
 * presets showed up in the template dialog with no way to remove one:
 * `delete_sync_profile_cmd` was registered and never called.
 */
describe('saved sync presets can be deleted', () => {
    it('deletes a saved preset and returns the list the backend now holds', async () => {
        const invoke = vi.fn(async (cmd: string) => (cmd === 'load_sync_profiles_cmd' ? [profile('mirror', true)] : undefined));
        const shown = [profile('mirror', true), profile('nightly-imported', false)];
        const { left, reloadError } = await deleteSavedSyncProfile(invoke as never, profile('nightly-imported', false), shown);
        expect(invoke).toHaveBeenNthCalledWith(1, 'delete_sync_profile_cmd', { id: 'nightly-imported' });
        expect(invoke).toHaveBeenNthCalledWith(2, 'load_sync_profiles_cmd');
        expect(left.map((p) => p.id)).toEqual(['mirror']);
        expect(reloadError).toBeNull();
    });

    it('drops the deleted preset from the list even when the reload fails', async () => {
        // The delete went through; a failed reload must not leave the gone
        // preset selectable, where Export would send its id.
        const invoke = vi.fn(async (cmd: string) => {
            if (cmd === 'load_sync_profiles_cmd') throw new Error('vault busy');
            return undefined;
        });
        const shown = [profile('mirror', true), profile('nightly-imported', false)];
        const { left, reloadError } = await deleteSavedSyncProfile(invoke as never, profile('nightly-imported', false), shown);
        expect(left.map((p) => p.id)).toEqual(['mirror']);
        expect(reloadError).toContain('vault busy');
    });

    it('never sends a built-in preset to the backend', async () => {
        const invoke = vi.fn();
        expect(isDeletableSyncProfile(profile('mirror', true))).toBe(false);
        await expect(deleteSavedSyncProfile(invoke as never, profile('mirror', true), [])).rejects.toThrow();
        expect(invoke).not.toHaveBeenCalled();
    });

    it('offers the delete in the template dialog, behind a confirmation', () => {
        expect(dialogSource).toContain('deleteSavedSyncProfile(invoke');
        expect(dialogSource).toContain('<ConfirmOverlay');
    });

    it('keeps a preset picked while the delete was running', () => {
        // The confirmation closes before the delete answers and the selector
        // stays enabled: the answer must not overwrite a newer choice.
        expect(dialogSource).toMatch(/setPresetId\(current =>\s*left\.some\(\(?p\)? => p\.id === current\)/);
        expect(dialogSource).toMatch(/if \(reloadError\) setResult\(\{ success: false, message: reloadError \}\);/);
    });
});
