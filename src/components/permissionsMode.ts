// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Read, write and execute for one class (owner, group or others). */
export interface PermissionTriple {
    read: boolean;
    write: boolean;
    execute: boolean;
}

export interface PermissionFlags {
    owner: PermissionTriple;
    group: PermissionTriple;
    others: PermissionTriple;
}

export interface PermissionState {
    octal: string;
    flags: PermissionFlags;
    /** False when the listing gave nothing this dialog can read as a mode. */
    known: boolean;
}

const DEFAULT_OCTAL = '644';

/** Exactly three octal digits: the only value the dialog sends. */
export const isCompleteOctal = (value: string): boolean => /^[0-7]{3}$/.test(value);

export const flagsFromOctal = (octal: string): PermissionFlags => {
    const digit = (d: string): PermissionTriple => {
        const v = Number.parseInt(d, 8);
        return { read: (v & 4) !== 0, write: (v & 2) !== 0, execute: (v & 1) !== 0 };
    };
    return { owner: digit(octal[0]), group: digit(octal[1]), others: digit(octal[2]) };
};

export const octalFromFlags = (flags: PermissionFlags): string => {
    const value = (t: PermissionTriple) => (t.read ? 4 : 0) + (t.write ? 2 : 0) + (t.execute ? 1 : 0);
    return `${value(flags.owner)}${value(flags.group)}${value(flags.others)}`;
};

/**
 * The dialog's starting state from what the listing reported: `rwx` letters
 * with or without the type letter (`-rw-r--r--`, `rw-r--r--`, a trailing ACL
 * marker allowed) or octal (`644`, `0644`). Anything else, such as MLSD's
 * `perm` fact (`adfrw`), is not a mode: the dialog starts from 644 and says
 * the current mode is unknown instead of presenting a guess as the file's.
 */
export const parsePermissions = (current?: string | null): PermissionState => {
    const text = (current ?? '').trim().replace(/[+@.]$/, '');
    const letters = text.length === 10 ? text.slice(1) : text;
    if (/^[r-][w-][xsS-][r-][w-][xsS-][r-][w-][xtT-]$/.test(letters)) {
        const triple = (s: string): PermissionTriple => ({ read: s[0] === 'r', write: s[1] === 'w', execute: s[2] === 'x' || s[2] === 's' || s[2] === 't' });
        const flags = { owner: triple(letters.slice(0, 3)), group: triple(letters.slice(3, 6)), others: triple(letters.slice(6, 9)) };
        return { octal: octalFromFlags(flags), flags, known: true };
    }
    const octal = /^0?[0-7]{3}$/.test(text) ? text.slice(-3) : null;
    if (octal) return { octal, flags: flagsFromOctal(octal), known: true };
    return { octal: DEFAULT_OCTAL, flags: flagsFromOctal(DEFAULT_OCTAL), known: false };
};

/** Flip one permission without mutating the flags it was given. */
export const toggleFlag = (flags: PermissionFlags, section: keyof PermissionFlags, kind: keyof PermissionTriple): PermissionFlags => ({
    ...flags,
    [section]: { ...flags[section], [kind]: !flags[section][kind] },
});
