// SPDX-License-Identifier: GPL-3.0-or-later

import { describe, expect, it } from 'vitest';
import {
    FILEN_BRIDGE_MAX_LENGTH,
    FILEN_BRIDGE_REJECTED_CHARACTERS,
    PASSWORD_PRESETS,
    filenBridgeCredentialError,
    passwordCommandArgs,
} from './passwordForge';

describe('passwordForge', () => {
    it('defines a compatible 32 preset that satisfies the Filen bridge restrictions', () => {
        const preset = PASSWORD_PRESETS.compatible;
        expect(preset.length).toBe(FILEN_BRIDGE_MAX_LENGTH);
        expect([...preset.customCharacters].some(char => FILEN_BRIDGE_REJECTED_CHARACTERS.includes(char))).toBe(false);
        expect(passwordCommandArgs(preset).requireEachGroup).toBe(true);
    });

    it('validates Filen bridge length and rejected characters', () => {
        expect(filenBridgeCredentialError('A'.repeat(33))).toBe('tooLong');
        expect(filenBridgeCredentialError('valid-but-rejected')).toBe('rejected');
        expect(filenBridgeCredentialError('Valid!BridgeKey23')).toBeNull();
    });
});
