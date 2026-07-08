// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { archiveKindForName } from '../utils/archiveCipher';

/**
 * Lazy, cached proactive archive-encryption detection for the list-view badges
 * (the Type-column padlock and the optional Encryption column).
 *
 * Detection reads an archive header via the backend detect_archive_meta command,
 * so it only runs when a caller opts in (a relevant column is visible), only for
 * archive-type files, capped per directory, with limited concurrency to avoid an
 * IPC storm. Results are cached module-wide keyed by path + size + mtime, so
 * revisiting a directory is instant and an edited file re-detects.
 */

export type ArchiveMetaState =
    | { status: 'loading' }
    | { status: 'unknown' }
    | {
          status: 'done';
          encrypted: boolean;
          cipher: string | null;
          compression: string | null;
          /** AeroVault container generation (`v2`/`v3`/`v4`) for the Type-column
           *  badge; null for non-vault entries. */
          vaultVersion: string | null;
      };

interface FileLike {
    path: string;
    name: string;
    size: number | null;
    modified: string | null;
    is_dir: boolean;
}

interface CachedMeta {
    encrypted: boolean;
    cipher: string | null;
    /** Compression method token (e.g. `Deflate`, `Zstd`), or null for a stored
     *  archive / a container whose method cannot be read proactively. */
    compression: string | null;
    /** AeroVault generation (`v2`/`v3`/`v4`), or null for non-vault entries. */
    vaultVersion: string | null;
    /** False when detection failed or the format is not proactively detectable. */
    known: boolean;
}

const CACHE = new Map<string, CachedMeta>();
const MAX_DETECT = 400;
const CONCURRENCY = 8;

function cacheKey(f: FileLike, remote: boolean): string {
    // Scope by local/remote so a local path and a remote path that happen to
    // collide (e.g. both "/foo/bar.aerovault") never share a cache entry.
    return `${remote ? 'r' : 'l'}|${f.path}|${f.size ?? ''}|${f.modified ?? ''}`;
}

/**
 * How a given file's encryption metadata is obtained. Third-party archives
 * (zip/7z/rar) go through `detect_archive_meta`; our own container formats go
 * through the cheap `detect_aero_container` magic sniff (.aerovault = encrypted
 * AES-256, .aerozip = the plaintext Zip lane); our exported bundles
 * (.aeroftp-keystore keystore, .aeroftp profile backup) are AES-256-GCM
 * encrypted by construction, so they need no read at all.
 */
type DetectPlan =
    | { via: 'archive'; kind: 'zip' | 'sevenz' | 'rar' }
    | { via: 'aero' }
    | { via: 'sealed' };

function detectionPlan(name: string, remote = false): DetectPlan | null {
    const kind = archiveKindForName(name);
    if (kind === 'zip' || kind === 'sevenz' || kind === 'rar') {
        // Third-party ZIP/7z/RAR live-detection on a remote needs head+tail ranged
        // reads (their index is at the file tail) and is a later phase; on remote we
        // surface only our own head-magic container formats for now. Locally it is
        // the full path-based detector.
        return remote ? null : { via: 'archive', kind };
    }
    const lower = name.toLowerCase();
    if (lower.endsWith('.aerovault') || lower.endsWith('.aerozip')) return { via: 'aero' };
    // Our exported bundles are always AES-256-GCM encrypted (order matters:
    // `.aeroftp-keystore` must be tested before the `.aeroftp` suffix).
    if (lower.endsWith('.aeroftp-keystore') || lower.endsWith('.aeroftp')) return { via: 'sealed' };
    return null;
}

type DetectResult = {
    encrypted: boolean;
    cipher: string | null;
    compression: string | null;
    vaultVersion: string | null;
};

/** Resolve one file's encryption + compression (+ AeroVault version) metadata via
 *  the backend path its plan selects. `remote` routes the container sniff through
 *  the provider ranged-read command (which fetches only the header, never the whole
 *  file) instead of the local filesystem detector. */
function runDetection(plan: DetectPlan, path: string, remote: boolean): Promise<DetectResult> {
    if (plan.via === 'archive') {
        // Remote plans never carry `via: 'archive'` (detectionPlan gates it), so this
        // is always the local filesystem detector.
        return invoke<{ encrypted: boolean; cipher: string | null; compression: string | null }>(
            'detect_archive_meta',
            { archivePath: path, kind: plan.kind },
        ).then((m) => ({ ...m, vaultVersion: null }));
    }
    if (plan.via === 'aero') {
        if (remote) {
            // Ranged header sniff over the provider: same classification as the local
            // detect_aero_container + detect_aero_vault_version, no whole-file fetch.
            // A NotSupported provider rejects -> cached as unknown -> no badge.
            return invoke<{ container: string | null; version: string | null }>(
                'provider_detect_aero_remote',
                { remotePath: path },
            ).then(({ container, version }) => {
                if (container === 'zip') {
                    return { encrypted: false, cipher: null, compression: 'Zstd', vaultVersion: null };
                }
                if (container === 'vault') {
                    return { encrypted: true, cipher: 'AES-256', compression: 'Zstd', vaultVersion: version ?? null };
                }
                throw new Error('not an aero container');
            });
        }
        // detect_aero_container sniffs the AeroVault family header: "vault" is an
        // encrypted AES-256 container, "zip" is the plaintext .aerozip lane, null
        // is not one of ours (renamed/corrupt) -> undetectable, no badge. Both
        // lanes are AeroVault v3, which always compresses per-chunk with Zstd. For
        // an actual vault we also read the generation (v2/v3/v4) for the Type badge.
        return invoke<string | null>('detect_aero_container', { path }).then((kind) => {
            if (kind === 'zip') {
                return { encrypted: false, cipher: null, compression: 'Zstd', vaultVersion: null };
            }
            if (kind === 'vault') {
                return invoke<string | null>('detect_aero_vault_version', { path }).then((v) => ({
                    encrypted: true,
                    cipher: 'AES-256',
                    compression: 'Zstd',
                    vaultVersion: v ?? null,
                }));
            }
            throw new Error('not an aero container');
        });
    }
    // sealed: an exported .aeroftp-keystore / .aeroftp bundle is always AES-256
    // encrypted; the payload is an opaque (uncompressed) encrypted blob, so it
    // carries an emerald cipher badge but no compression / version badge.
    return Promise.resolve({ encrypted: true, cipher: 'AES-256', compression: null, vaultVersion: null });
}

export function useArchiveMeta(files: FileLike[], enabled: boolean, remote = false) {
    const [version, setVersion] = useState(0);
    const inflight = useRef<Set<string>>(new Set());

    useEffect(() => {
        if (!enabled) return;
        let cancelled = false;
        const targets = files
            .filter((f) => !f.is_dir && detectionPlan(f.name, remote))
            .slice(0, MAX_DETECT)
            .filter((f) => {
                const k = cacheKey(f, remote);
                return !CACHE.has(k) && !inflight.current.has(k);
            });
        if (targets.length === 0) return;

        let idx = 0;
        let active = 0;
        let done = 0;
        const total = targets.length;

        const pump = () => {
            while (active < CONCURRENCY && idx < targets.length) {
                const f = targets[idx++];
                const k = cacheKey(f, remote);
                const plan = detectionPlan(f.name, remote)!;
                inflight.current.add(k);
                active++;
                runDetection(plan, f.path, remote)
                    .then((meta) => {
                        CACHE.set(k, {
                            encrypted: !!meta.encrypted,
                            cipher: meta.cipher ?? null,
                            compression: meta.compression ?? null,
                            vaultVersion: meta.vaultVersion ?? null,
                            known: true,
                        });
                    })
                    .catch(() => {
                        // Undetectable (truncated, unreadable): neutral, not a wrong badge.
                        CACHE.set(k, {
                            encrypted: false,
                            cipher: null,
                            compression: null,
                            vaultVersion: null,
                            known: false,
                        });
                    })
                    .finally(() => {
                        inflight.current.delete(k);
                        active--;
                        done++;
                        if (!cancelled && (done === total || done % CONCURRENCY === 0)) {
                            setVersion((v) => v + 1);
                        }
                        if (!cancelled) pump();
                    });
            }
        };
        pump();
        return () => {
            cancelled = true;
        };
    }, [files, enabled, remote]);

    return useCallback(
        (f: FileLike): ArchiveMetaState | undefined => {
            if (!enabled || f.is_dir || !detectionPlan(f.name, remote)) return undefined;
            const hit = CACHE.get(cacheKey(f, remote));
            if (!hit) return { status: 'loading' };
            if (!hit.known) return { status: 'unknown' };
            return {
                status: 'done',
                encrypted: hit.encrypted,
                cipher: hit.cipher,
                compression: hit.compression,
                vaultVersion: hit.vaultVersion,
            };
        },
        // version bumps as results land, giving the lookup a fresh identity so
        // consumers re-render and read the newly-cached entries.
        [enabled, version, remote],
    );
}
