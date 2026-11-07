// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';

/**
 * Discussion #1527: the Settings panel closed itself on ESC from its own
 * keydown handler, before the TOTP setup opened over it could ask. The ESC
 * branch of that handler is run from SettingsPanel.tsx's source (the panel
 * is 4000 lines over some 60 modules, too many to mock for one key), and the
 * test checks that the panel still wires the handler.
 */
const source = (): string => readFileSync(resolve(process.cwd(), 'src/components/SettingsPanel.tsx'), 'utf8');

const escapeBranch = (): string => {
    const src = source();
    const start = src.indexOf('const handlePanelKeyDown = useCallback(');
    expect(start).toBeGreaterThan(-1);
    const from = src.slice(start).search(/if \(e\.key === ['"]Escape['"]\) \{/);
    const to = src.slice(start).search(/if \(e\.key !== ['"]Tab['"]\) return;/);
    expect(from).toBeGreaterThan(-1);
    expect(to).toBeGreaterThan(from);
    return src.slice(start + from, start + to);
};

const press = (claimed: boolean) => {
    const onClose = vi.fn();
    const event = { key: 'Escape', preventDefault: vi.fn() };
    const scope = { onClose, isEscapeClaimed: () => claimed };
    new Function('scope', 'e', `with (scope) { ${escapeBranch()} }`)(scope, event);
    return { onClose, event };
};

describe('the Settings panel ESC', () => {
    it('is the handler the panel listens with', () => {
        expect(source()).toMatch(/onKeyDown=\{handlePanelKeyDown\}/);
    });

    it('leaves the key to a modal open over the panel', () => {
        const { onClose, event } = press(true);
        expect(onClose).not.toHaveBeenCalled();
        expect(event.preventDefault).not.toHaveBeenCalled();
    });

    it('closes the panel when nothing is open over it', () => {
        const { onClose } = press(false);
        expect(onClose).toHaveBeenCalledTimes(1);
    });
});
