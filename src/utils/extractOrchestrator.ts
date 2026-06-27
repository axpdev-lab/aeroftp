// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { runExtractWithToast } from './extractToast';

/**
 * Shared archive/vault extraction orchestration (Deliverable G). One source of
 * truth for: which backend command extracts which container kind, how to derive
 * an "Extract to folder" subfolder name, and how to probe encryption. Consumed
 * by BOTH the main app context menu (src/App.tsx) and the dedicated `/extract`
 * window (src/components/ExtractWindow.tsx) so the two never drift. Progress is
 * always driven through `runExtractWithToast`, never re-implemented.
 */

/** Container kind as reported by the backend `extract_probe` command. */
export type ArchiveKind =
    | 'zip'
    | 'sevenz'
    | 'rar'
    | 'tar'
    | 'aerozip'
    | 'aerovault_v2'
    | 'aerovault_v3';

/** General (non-aero) archive kinds, which share the `extract_*` command shape. */
export type GeneralArchiveKind = 'zip' | 'sevenz' | 'rar' | 'tar';

export interface ExtractProbe {
    kind: ArchiveKind;
    /** True when extraction needs a password (encrypted general archive, or any
     *  non-plaintext aero container). */
    encrypted: boolean;
    /** Archive size in bytes (for the toast threshold). */
    archive_bytes: number;
}

export interface ExtractOutcome {
    /** Largest determinate extracted byte total seen (0 when unknown). */
    extractedTotal: number;
    /** Entry count for aero lanes that report it; undefined for general formats. */
    count?: number;
}

type ToastOpts = { filename: string; archiveBytes: number | null | undefined };

/**
 * Strip the full archive extension from a file name, returning the stem used to
 * name an "Extract to folder" subfolder. Mirrors the Rust `archive_extract_stem`
 * (kept in lockstep): handles the multi-part tar extensions and the aero* +
 * general single extensions, falling back to a last-dot strip. Pure.
 */
export function archiveStem(fileName: string): string {
    const lower = fileName.toLowerCase();
    for (const ext of ['.tar.gz', '.tar.xz', '.tar.bz2']) {
        if (lower.endsWith(ext)) return fileName.slice(0, fileName.length - ext.length);
    }
    for (const ext of ['.tgz', '.txz', '.tbz2', '.tar', '.zip', '.7z', '.rar', '.aerozip', '.aerovault']) {
        if (lower.endsWith(ext)) return fileName.slice(0, fileName.length - ext.length);
    }
    const dot = fileName.lastIndexOf('.');
    return dot > 0 ? fileName.slice(0, dot) : fileName;
}

/**
 * Infer the general (non-aero) archive kind from a file name, or null if it is
 * not a recognized general archive. Pure: the single regex source the app and
 * the dedicated window share.
 */
export function inferGeneralKind(fileName: string): GeneralArchiveKind | null {
    if (/\.zip$/i.test(fileName)) return 'zip';
    if (/\.7z$/i.test(fileName)) return 'sevenz';
    if (/\.rar$/i.test(fileName)) return 'rar';
    if (/\.(tar|tar\.gz|tgz|tar\.xz|txz|tar\.bz2|tbz2)$/i.test(fileName)) return 'tar';
    return null;
}

/**
 * Encryption-routing decision for the dedicated window: a clear archive extracts
 * headlessly with no prompt; an encrypted one must collect a password first. Pure
 * (single source so the routing is unit-tested, not just inlined in the UI).
 */
export function needsPasswordPrompt(probe: Pick<ExtractProbe, 'encrypted'>): boolean {
    return probe.encrypted;
}

/** Probe an archive/vault for its kind + whether a password is required. */
export function probeArchive(path: string): Promise<ExtractProbe> {
    return invoke<ExtractProbe>('extract_probe', { path });
}

/**
 * Resolve a never-clobbering "Extract to folder" destination via the backend
 * (`parent/stem`, or `parent/stem (2)`, ... if earlier candidates exist).
 */
export function resolveUniqueExtractDir(parentDir: string, archiveName: string): Promise<string> {
    return invoke<string>('resolve_unique_extract_dir', { parentDir, archiveName });
}

/**
 * Dispatch a general (zip/7z/rar/tar) extraction. Matches the legacy App.tsx call
 * shape (`outputDir` + `createSubfolder`, the backend builds the stem subfolder)
 * so the app can delegate without behavior change; the dedicated window passes a
 * precomputed `outputDir` with `createSubfolder: false`.
 */
export async function dispatchGeneralExtract(params: {
    kind: GeneralArchiveKind;
    archivePath: string;
    outputDir: string;
    createSubfolder: boolean;
    password: string | null;
    toastOpts: ToastOpts;
}): Promise<ExtractOutcome> {
    const { kind, archivePath, outputDir, createSubfolder, password, toastOpts } = params;
    switch (kind) {
        case 'zip': {
            const { extractedTotal } = await runExtractWithToast<string>(
                'extract_archive',
                { archivePath, outputDir, createSubfolder, password },
                toastOpts,
            );
            return { extractedTotal };
        }
        case 'sevenz': {
            const { extractedTotal } = await runExtractWithToast<string>(
                'extract_7z',
                { archivePath, outputDir, password, createSubfolder },
                toastOpts,
            );
            return { extractedTotal };
        }
        case 'rar': {
            const { extractedTotal } = await runExtractWithToast<string>(
                'extract_rar',
                { archivePath, outputDir, password, createSubfolder },
                toastOpts,
            );
            return { extractedTotal };
        }
        case 'tar': {
            const { extractedTotal } = await runExtractWithToast<string>(
                'extract_tar',
                { archivePath, outputDir, createSubfolder },
                toastOpts,
            );
            return { extractedTotal };
        }
    }
}

/**
 * Dispatch an aero* (aerozip / aerovault v2 / v3) extraction into the exact
 * `destDir` (the caller precomputes any subfolder). `.aerozip` is plaintext and
 * ignores the password; the encrypted vault lanes require it.
 */
export async function dispatchAeroExtract(params: {
    kind: 'aerozip' | 'aerovault_v2' | 'aerovault_v3';
    archivePath: string;
    destDir: string;
    password: string | null;
    toastOpts: ToastOpts;
}): Promise<ExtractOutcome> {
    const { kind, archivePath, destDir, password, toastOpts } = params;
    switch (kind) {
        case 'aerozip': {
            const { result, extractedTotal } = await runExtractWithToast<number>(
                'aerovz_extract_all',
                { vaultPath: archivePath, destPath: destDir },
                toastOpts,
            );
            return { extractedTotal, count: result };
        }
        case 'aerovault_v2': {
            const { extractedTotal } = await runExtractWithToast<unknown>(
                'vault_v2_extract_all',
                { vaultPath: archivePath, password: password ?? '', destDir },
                toastOpts,
            );
            return { extractedTotal };
        }
        case 'aerovault_v3': {
            const { result, extractedTotal } = await runExtractWithToast<number>(
                'vault_v3_extract_all',
                { vaultPath: archivePath, password: password ?? '', destPath: destDir },
                toastOpts,
            );
            return { extractedTotal, count: result };
        }
    }
}

/**
 * Extract into the exact final `destDir` (no subfolder logic here). Used by the
 * dedicated `/extract` window, which precomputes `destDir` (the archive's own
 * directory for "Extract here", or a unique stem subfolder for "Extract to
 * folder"). Routes general vs aero kinds to the right dispatcher.
 */
export function extractToDir(params: {
    kind: ArchiveKind;
    archivePath: string;
    archiveName: string;
    archiveBytes: number | null | undefined;
    destDir: string;
    password: string | null;
}): Promise<ExtractOutcome> {
    const { kind, archivePath, archiveName, archiveBytes, destDir, password } = params;
    const toastOpts: ToastOpts = { filename: archiveName, archiveBytes };
    if (kind === 'aerozip' || kind === 'aerovault_v2' || kind === 'aerovault_v3') {
        return dispatchAeroExtract({ kind, archivePath, destDir, password, toastOpts });
    }
    return dispatchGeneralExtract({
        kind,
        archivePath,
        outputDir: destDir,
        createSubfolder: false,
        password,
        toastOpts,
    });
}
