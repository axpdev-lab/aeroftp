// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

describe('TypeScript configuration', () => {
    it('stays strict JSON for external tools', () => {
        const source = readFileSync('tsconfig.json', 'utf8');
        expect(() => JSON.parse(source)).not.toThrow();
    });
});
