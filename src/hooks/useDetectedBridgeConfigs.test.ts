// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Pins the bridge picker's autodetect: which tools come first, how their
// config path is shown, and the sweep's contract with its consumers.
//
// That last group exists because of a live failure the first version's gates
// could not see. The sweep used to live inside the effect behind a per-mount
// ref; under StrictMode the first pass was discarded by its own cleanup and
// the second returned early on the surviving ref, so the picker never showed
// a single detection in the running app while tsc, vitest and a by-hand call
// to the backend all said the code was fine. The probe itself is a Tauri round
// trip and still is not covered here: only a live run proves that end.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import {
    detectBridgeConfigBounded,
    detectedBridgeConfigsSoFar,
    loadDetectedBridgeConfigs,
    subscribeDetectedBridgeConfigs,
    orderBridgeSourcesByDetection,
    resetDetectedBridgeConfigsCache,
    shortenConfigPath,
} from './useDetectedBridgeConfigs';
import { GENERIC_BRIDGE_SOURCES } from '../components/bridge/bridgeSources';

const ids = (list: { id: string }[]) => list.map(s => s.id);

describe('orderBridgeSourcesByDetection', () => {
    it('floats detected tools to the top', () => {
        // cyberduck sits at index 10 of the curated list, dreamweaver at 11:
        // exactly the "hiding at position 11 of 15" case this exists to fix.
        const ordered = orderBridgeSourcesByDetection(GENERIC_BRIDGE_SOURCES, {
            cyberduck: '/home/me/Library/Application Support/Cyberduck/bookmark.duck',
            dreamweaver: '/home/me/sites/site.ste',
        });
        expect(ids(ordered).slice(0, 2)).toEqual(['cyberduck', 'dreamweaver']);
    });

    it('keeps the curated order inside each group', () => {
        const ordered = orderBridgeSourcesByDetection(GENERIC_BRIDGE_SOURCES, {
            filezilla: '/home/me/.config/filezilla/sitemanager.xml',
            lftp: '/home/me/.lftprc',
        });
        // Detected pair in curated order (filezilla is 3rd, lftp 10th), and the
        // untouched remainder still starts with rclone, winscp, aws.
        expect(ids(ordered).slice(0, 2)).toEqual(['filezilla', 'lftp']);
        expect(ids(ordered).slice(2, 5)).toEqual(['rclone', 'winscp', 'aws']);
    });

    it('leaves the list alone when nothing was detected, and does not mutate the input', () => {
        const before = ids(GENERIC_BRIDGE_SOURCES);
        expect(ids(orderBridgeSourcesByDetection(GENERIC_BRIDGE_SOURCES, {}))).toEqual(before);
        expect(ids(GENERIC_BRIDGE_SOURCES)).toEqual(before);
    });
});

describe('shortenConfigPath', () => {
    it('keeps the two segments that identify the tool', () => {
        expect(shortenConfigPath('/home/me/.config/filezilla/sitemanager.xml')).toBe('…/filezilla/sitemanager.xml');
    });

    it('handles Windows paths', () => {
        expect(shortenConfigPath('C:\\Users\\me\\AppData\\Roaming\\FileZilla\\sitemanager.xml')).toBe('…/FileZilla/sitemanager.xml');
    });

    it('returns short paths untouched rather than prefixing an ellipsis to nothing', () => {
        expect(shortenConfigPath('/etc/s3cfg')).toBe('/etc/s3cfg');
        expect(shortenConfigPath('.lftprc')).toBe('.lftprc');
    });
});

describe('loadDetectedBridgeConfigs', () => {
    beforeEach(resetDetectedBridgeConfigsCache);

    const found: Record<string, string> = {
        rclone: '/home/me/.config/rclone/rclone.conf',
        ssh: '/home/me/.ssh/config',
    };
    const probeInstalled = async (id: string) => found[id] ?? null;

    it('reports the tools that have a config here and omits the rest', async () => {
        expect(await loadDetectedBridgeConfigs(probeInstalled)).toEqual(found);
    });

    it('serves a consumer that arrives while the sweep is still running', async () => {
        // The live failure this pins: the dialog mounts, the sweep starts, the
        // mount is thrown away and redone (StrictMode does exactly this), and
        // the second consumer must still receive the answer. The first version
        // guarded the sweep with a per-mount ref, so the second consumer got
        // nothing at all and the picker stayed blank in the running app while
        // every gate here was green.
        let release: (() => void) | undefined;
        const gate = new Promise<void>(resolve => { release = resolve; });
        let calls = 0;
        const slowProbe = async (id: string) => {
            calls += 1;
            await gate;
            return found[id] ?? null;
        };

        const first = loadDetectedBridgeConfigs(slowProbe);
        const second = loadDetectedBridgeConfigs(slowProbe);
        release?.();

        expect(await second).toEqual(found);
        expect(await first).toEqual(await second);
        // One sweep, not two: the late consumer joined it.
        expect(calls).toBe(GENERIC_BRIDGE_SOURCES.length);
    });

    it('probes once per app run, then answers from the cache', async () => {
        let calls = 0;
        const counting = async (id: string) => { calls += 1; return found[id] ?? null; };
        await loadDetectedBridgeConfigs(counting);
        await loadDetectedBridgeConfigs(counting);
        expect(calls).toBe(GENERIC_BRIDGE_SOURCES.length);
    });

    it('keeps going when one probe throws', async () => {
        // rclone shells out to `rclone config file`; a machine without rclone
        // must cost that one row, not the whole list.
        const flaky = async (id: string) => {
            if (id === 'rclone') throw new Error('rclone not installed');
            return found[id] ?? null;
        };
        expect(await loadDetectedBridgeConfigs(flaky)).toEqual({ ssh: found.ssh });
    });
});

describe('results arrive in pieces', () => {
    beforeEach(resetDetectedBridgeConfigsCache);

    /** Probe that answers `fast` immediately and never answers for `hangs`. */
    const hangingProbe = (found: Record<string, string>, hangs: string) =>
        async (id: string): Promise<string | null> => {
            if (id === hangs) return new Promise<string | null>(() => { /* never settles */ });
            return found[id] ?? null;
        };

    it('publishes a detection before the sweep is over', async () => {
        // The assertion is the TIMING, not the content: a version that
        // collected everything and published once at the end would satisfy
        // "ssh was seen" just as well, while leaving the picker blank for as
        // long as the slowest probe takes. So the sweep is held open on a
        // probe that has not answered yet, and the update has to arrive anyway.
        const seen: Record<string, string>[] = [];
        subscribeDetectedBridgeConfigs(found => seen.push(found));

        let release: (() => void) | undefined;
        const held = new Promise<void>(resolve => { release = resolve; });
        const sweep = loadDetectedBridgeConfigs(async id => {
            if (id === 'ssh') return '/home/me/.ssh/config';
            if (id === 'restic') { await held; return null; }
            return null;
        });

        await new Promise(resolve => setTimeout(resolve, 0));
        // restic has not answered, so the sweep cannot have finished.
        expect(seen).toEqual([{ ssh: '/home/me/.ssh/config' }]);

        release?.();
        await sweep;
    });

    it('a probe that never answers does not bury the ones that did', async () => {
        // rclone shells out to a binary; if that binary hangs, the promise from
        // loadDetectedBridgeConfigs never resolves. The other fourteen tools
        // must still reach the picker, so the answer cannot be final-only.
        const updates: Record<string, string>[] = [];
        subscribeDetectedBridgeConfigs(found => updates.push(found));
        void loadDetectedBridgeConfigs(hangingProbe({ ssh: '/home/me/.ssh/config' }, 'rclone'));

        // Let the workers drain everything that can answer.
        await new Promise(resolve => setTimeout(resolve, 0));

        expect(updates[updates.length - 1]).toEqual({ ssh: '/home/me/.ssh/config' });
        expect(detectedBridgeConfigsSoFar()).toEqual({ ssh: '/home/me/.ssh/config' });
    });

    it('stops talking to a listener that unsubscribed', async () => {
        const seen: Record<string, string>[] = [];
        const unsubscribe = subscribeDetectedBridgeConfigs(found => seen.push(found));
        unsubscribe();
        await loadDetectedBridgeConfigs(async id => (id === 'ssh' ? '/home/me/.ssh/config' : null));
        expect(seen).toEqual([]);
    });
});

describe('detectBridgeConfigBounded', () => {
    beforeEach(resetDetectedBridgeConfigsCache);
    afterEach(() => { vi.useRealTimers(); });

    it('answers from the sweep when it already knows, without probing again', async () => {
        await loadDetectedBridgeConfigs(async id => (id === 'ssh' ? '/home/me/.ssh/config' : null));
        let probed = false;
        const path = await detectBridgeConfigBounded('ssh', async () => { probed = true; return null; });
        expect(path).toBe('/home/me/.ssh/config');
        expect(probed).toBe(false);
    });

    it('gives up instead of spinning forever when the probe never answers', async () => {
        // The panel shows "Detecting..." until this resolves, and its browse
        // button lives behind that spinner: a probe that hangs used to strand
        // the user there with nothing to click.
        vi.useFakeTimers();
        const pending = detectBridgeConfigBounded('rclone', () => new Promise(() => { /* never */ }), 6000);
        await vi.advanceTimersByTimeAsync(6000);
        expect(await pending).toBe('');
    });

    it('reports a real answer as itself, and a failure as no config', async () => {
        expect(await detectBridgeConfigBounded('ssh', async () => '/home/me/.ssh/config')).toBe('/home/me/.ssh/config');
        expect(await detectBridgeConfigBounded('lftp', async () => null)).toBe('');
        expect(await detectBridgeConfigBounded('putty', async () => { throw new Error('nope'); })).toBe('');
    });
});
