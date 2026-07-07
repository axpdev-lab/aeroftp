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
 *
 * RAR is intentionally excluded: the backend defers proactive RAR detection, so
 * the lookup returns an "unknown" state (no padlock) rather than a wrong badge.
 */

export type ArchiveMetaState =
    | { status: 'loading' }
    | { status: 'unknown' }
    | { status: 'done'; encrypted: boolean; cipher: string | null };

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
    /** False when detection failed or the format is not proactively detectable. */
    known: boolean;
}

const CACHE = new Map<string, CachedMeta>();
const MAX_DETECT = 400;
const CONCURRENCY = 8;

function cacheKey(f: FileLike): string {
    return `${f.path}|${f.size ?? ''}|${f.modified ?? ''}`;
}

/** Kinds the backend can detect proactively (RAR deferred). */
function detectableKind(name: string): 'zip' | 'sevenz' | null {
    const kind = archiveKindForName(name);
    return kind === 'zip' || kind === 'sevenz' ? kind : null;
}

export function useArchiveMeta(files: FileLike[], enabled: boolean) {
    const [version, setVersion] = useState(0);
    const inflight = useRef<Set<string>>(new Set());

    useEffect(() => {
        if (!enabled) return;
        let cancelled = false;
        const targets = files
            .filter((f) => !f.is_dir && detectableKind(f.name))
            .slice(0, MAX_DETECT)
            .filter((f) => {
                const k = cacheKey(f);
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
                const k = cacheKey(f);
                const kind = detectableKind(f.name)!;
                inflight.current.add(k);
                active++;
                invoke<{ encrypted: boolean; cipher: string | null }>('detect_archive_meta', {
                    archivePath: f.path,
                    kind,
                })
                    .then((meta) => {
                        CACHE.set(k, { encrypted: !!meta.encrypted, cipher: meta.cipher ?? null, known: true });
                    })
                    .catch(() => {
                        // Undetectable (truncated, unreadable): neutral, not a wrong badge.
                        CACHE.set(k, { encrypted: false, cipher: null, known: false });
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
    }, [files, enabled]);

    return useCallback(
        (f: FileLike): ArchiveMetaState | undefined => {
            if (!enabled || f.is_dir || !detectableKind(f.name)) return undefined;
            const hit = CACHE.get(cacheKey(f));
            if (!hit) return { status: 'loading' };
            if (!hit.known) return { status: 'unknown' };
            return { status: 'done', encrypted: hit.encrypted, cipher: hit.cipher };
        },
        // version bumps as results land, giving the lookup a fresh identity so
        // consumers re-render and read the newly-cached entries.
        [enabled, version],
    );
}
