// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it, vi } from 'vitest';

/**
 * Discussion #1527: the Settings panel closed itself on ESC from its own
 * keydown handler, before the TOTP setup opened over it could ask. The ESC
 * branch of that handler is run from SettingsPanel.tsx's source.
 */
const escapeBranch = (): string => {
    const src = readFileSync(resolve(process.cwd(), 'src/components/SettingsPanel.tsx'), 'utf8');
    const start = src.indexOf('const handlePanelKeyDown = useCallback(');
    expect(start).toBeGreaterThan(-1);
    const from = src.indexOf("if (e.key === 'Escape') {", start);
    const to = src.indexOf("if (e.key !== 'Tab') return;", from);
    expect(from).toBeGreaterThan(start);
    expect(to).toBeGreaterThan(from);
    return src.slice(from, to);
};

const press = (claimed: boolean) => {
    const onClose = vi.fn();
    const event = { key: 'Escape', preventDefault: vi.fn() };
    const scope = { onClose, isEscapeClaimed: () => claimed };
    new Function('scope', 'e', `with (scope) { ${escapeBranch()} }`)(scope, event);
    return { onClose, event };
};

describe('the Settings panel ESC', () => {
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
