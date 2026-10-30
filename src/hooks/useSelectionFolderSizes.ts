// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef, useState } from 'react';

/**
 * Counts the folders of a local selection's Properties, one after the other:
 * each walk already keeps the disk busy with its own threads, and the total
 * grows as each folder is counted. True while folders are still being counted.
 *
 * A folder whose count fails keeps an unknown size, which the dialog already
 * reports, and the next folder is counted.
 *
 * The count starts again only when the folders change. `calculate` is read
 * through a ref: App's callback changes identity on most renders, and a count
 * keyed on it restarted at every progress event, said "done" while a folder
 * was still being counted, and counted each folder again once it finished.
 */
export function useSelectionFolderSizes(
    folders: readonly string[],
    calculate: (path: string) => Promise<void>,
): boolean {
    const calculateRef = useRef(calculate);
    useEffect(() => {
        calculateRef.current = calculate;
    });
    // NUL is the one character no path can hold.
    const key = folders.join('\0');
    const [running, setRunning] = useState(false);
    useEffect(() => {
        if (!key) return undefined;
        let closed = false;
        setRunning(true);
        void (async () => {
            for (const path of key.split('\0')) {
                if (closed) return;
                try {
                    await calculateRef.current(path);
                } catch {
                    // Its size stays unknown; the rest are still counted.
                }
            }
            if (!closed) setRunning(false);
        })();
        return () => {
            closed = true;
            setRunning(false);
        };
    }, [key]);
    return running;
}
