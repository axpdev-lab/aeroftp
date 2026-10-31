// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';

/**
 * Each local panel reloads when what it shows changes on disk.
 *
 * The watcher (src-tauri/src/local_panel_watcher.rs, its logic and inotify
 * tests there) watched panel 1's folder only, non-recursively: a flattened
 * search showed the folders below it and kept their rows when one of them
 * changed, and panel 2 never reloaded. Panel 1 did not reload either: the
 * listener's cleanup stopped the watch, and the listener subscribed again on
 * every render of App.
 */
describe('each local panel reloads when what it shows changes on disk', () => {
    const count = (text: string, re: RegExp) => [...text.matchAll(re)].length;
    const section = (() => {
        const start = appRaw.indexOf('// AeroFile auto-refresh:');
        expect(start).toBeGreaterThan(-1);
        return appRaw.slice(start, appRaw.indexOf('\n  // Ehud #2:', start));
    })();
    /** The effect that listens for the watcher's events, whatever its shape. */
    const listener = (() => {
        const event = appRaw.indexOf("'local-fs-changed'");
        expect(event).toBeGreaterThan(-1);
        expect(appRaw.indexOf("'local-fs-changed'", event + 1)).toBe(-1);
        const start = appRaw.lastIndexOf('\n  useEffect(() => {', event);
        const end = appRaw.indexOf('\n  }, [', event);
        return { body: appRaw.slice(start, end), deps: appRaw.slice(end, appRaw.indexOf('\n', end + 1)) };
    })();

    it('watches the folders a flattened search lists below panel 1\'s', () => {
        expect(section).toMatch(/flattenSearchParse\.active && localFlattenedFiles\s*\? localFlattenedFiles\.filter\(f => f\.is_dir\)\.map\(f => f\.path\)\s*: \[\],/);
        expect(section).toMatch(/invoke\('local_panel_watch', \{ path: currentLocalPath, panel: 'local', dirs \}\)/);
        expect(section).toMatch(/\}, \[currentLocalPath, localWatchDirs\]\);/);
    });

    it('watches panel 2 while it is on screen', () => {
        expect(section).toMatch(/if \(localPanel2Visible && currentLocalPath2\) \{\s*void invoke\('local_panel_watch', \{ path: currentLocalPath2, panel: 'local2' \}\)/);
        expect(section).toMatch(/\} else \{\s*void invoke\('local_panel_watch_stop', \{ panel: 'local2' \}\)/);
        expect(section).toMatch(/\}, \[localPanel2Visible, currentLocalPath2\]\);/);
    });

    it('reloads the panel the change is for, at the folder it shows now', () => {
        expect(section).toMatch(/const panel = event\.payload\.panel === 'local2' \? 'local2' : 'local';/);
        expect(section).toMatch(/const pathRef = panel === 'local2' \? currentLocalPath2Ref : currentLocalPathRef;/);
        expect(section).toMatch(/if \(!expected \|\| event\.payload\.path !== expected\) return;/);
        expect(listener.body).toMatch(/localLoadersRef\.current\[panel\]\(pathRef\.current\)/);
        expect(section).toMatch(/useLayoutEffect\(\(\) => \{ localLoadersRef\.current = \{ local: loadLocalFiles, local2: loadLocalFiles2 \}; \}\);/);
        // A change in panel 2 does not cancel panel 1's reload, nor the reverse.
        expect(section).toMatch(/pending\[panel\] = window\.setTimeout\(/);
    });

    it('subscribes once, and its cleanup leaves the watches alone', () => {
        // The listener followed the loaders, which change on every render of
        // App (they follow `notify`). Its cleanup stopped every watch, which
        // only a new folder started again; without the stop, it still dropped
        // the reload waiting to run.
        expect(listener.deps).toBe('\n  }, []);');
        expect(listener.body).toMatch(/return \(\) => \{/);
        expect(listener.body).not.toMatch(/local_panel_watch_stop/);
        // The one stop left is panel 2's, when it goes off screen.
        expect(count(appRaw, /local_panel_watch_stop/g)).toBe(1);
    });
});
