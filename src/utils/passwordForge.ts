// SPDX-License-Identifier: GPL-3.0-or-later

export type PasswordPresetId = 'balanced' | 'maximum' | 'compatible';
export type SymbolGroupId = 'punctuation' | 'brackets' | 'separators' | 'special';

export interface PasswordForgeSettings {
    length: number;
    uppercase: boolean;
    lowercase: boolean;
    digits: boolean;
    symbols: boolean;
    excludeAmbiguous: boolean;
    symbolGroups: SymbolGroupId[];
    customCharacters: string;
    excludedCharacters: string;
    requireEachGroup: boolean;
}

export const FILEN_BRIDGE_MAX_LENGTH = 32;
export const FILEN_BRIDGE_REJECTED_CHARACTERS = `.,:;"'\\/|_-`;
export const FILEN_SAFE_SYMBOLS = '!@#$%^&*()[]{}<>?~+=';

export const PASSWORD_PRESETS: Record<PasswordPresetId, PasswordForgeSettings> = {
    balanced: {
        length: 24,
        uppercase: true,
        lowercase: true,
        digits: true,
        symbols: true,
        excludeAmbiguous: true,
        symbolGroups: ['punctuation', 'brackets', 'separators', 'special'],
        customCharacters: '',
        excludedCharacters: '',
        requireEachGroup: true,
    },
    maximum: {
        length: 40,
        uppercase: true,
        lowercase: true,
        digits: true,
        symbols: true,
        excludeAmbiguous: false,
        symbolGroups: ['punctuation', 'brackets', 'separators', 'special'],
        customCharacters: '',
        excludedCharacters: '',
        requireEachGroup: true,
    },
    compatible: {
        length: FILEN_BRIDGE_MAX_LENGTH,
        uppercase: true,
        lowercase: true,
        digits: true,
        symbols: false,
        excludeAmbiguous: true,
        symbolGroups: [],
        customCharacters: FILEN_SAFE_SYMBOLS,
        excludedCharacters: FILEN_BRIDGE_REJECTED_CHARACTERS,
        requireEachGroup: true,
    },
};

export function passwordCommandArgs(settings: PasswordForgeSettings, count = 1) {
    return {
        length: settings.length,
        uppercase: settings.uppercase,
        lowercase: settings.lowercase,
        digits: settings.digits,
        symbols: settings.symbols,
        excludeAmbiguous: settings.excludeAmbiguous,
        count,
        symbolGroups: settings.symbolGroups,
        customCharacters: settings.customCharacters || null,
        excludedCharacters: settings.excludedCharacters || null,
        requireEachGroup: settings.requireEachGroup,
    };
}

export function entropyCommandArgs(settings: PasswordForgeSettings) {
    const { count: _count, requireEachGroup: _required, ...args } = passwordCommandArgs(settings);
    return args;
}

export function filenBridgeCredentialError(value: string): 'tooLong' | 'rejected' | null {
    if (value.length > FILEN_BRIDGE_MAX_LENGTH) return 'tooLong';
    return [...value].some(char => FILEN_BRIDGE_REJECTED_CHARACTERS.includes(char))
        ? 'rejected'
        : null;
}
