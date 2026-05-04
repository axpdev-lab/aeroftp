// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Vitest config kept separate from `vite.config.ts` so the Monaco asset
// copying plugin (which spins up a dev-server middleware and reads from
// node_modules) does not run during unit tests. The current scope is pure-TS
// utility logic (`src/utils/**/*.test.ts`); when component tests land we will
// add `environment: 'jsdom'` and the @testing-library setup to a separate
// project entry.

import { defineConfig } from 'vitest/config';

export default defineConfig({
    test: {
        environment: 'node',
        globals: false,
        include: ['src/**/*.test.ts'],
        exclude: [
            'node_modules/**',
            'dist/**',
            'src-tauri/**',
            'docs/**',
        ],
    },
});
