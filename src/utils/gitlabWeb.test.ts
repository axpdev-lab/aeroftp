// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import appSource from '../App.tsx?raw';
import { openOnGitLab } from './gitlabWeb';

/** The GitLab single-selection block of the remote context menu, from its guard to its closing brace. */
const gitlabMenuBlock = (src: string): string => {
    const start = src.indexOf("if (currentProtocol === 'gitlab' && filesToUse.length === 1) {");
    const end = src.indexOf('\n    }\n', start);
    expect(start).toBeGreaterThan(-1);
    expect(end).toBeGreaterThan(start);
    return src.slice(start, end);
};

/**
 * The GitLab Tier 1 plan lists "View on GitLab" as done and the backend has
 * `gitlab_get_web_url`, but no menu ever called it: GitHub files had View on
 * GitHub, GitLab files had nothing.
 */
describe('View on GitLab', () => {
    it('asks the backend for the URL and opens it', async () => {
        const invoke = vi.fn(async () => 'https://gitlab.example.com/g/p/-/blob/main/docs/a%20b.md');
        const open = vi.fn(async () => {});
        await openOnGitLab(invoke as never, open, '/docs/a b.md', false);
        expect(invoke).toHaveBeenCalledWith('gitlab_get_web_url', { path: '/docs/a b.md', isDir: false });
        expect(open).toHaveBeenCalledWith('https://gitlab.example.com/g/p/-/blob/main/docs/a%20b.md');
    });

    it('is offered in the GitLab context menu, for one selected entry', () => {
        // Label and action are checked inside the one item the GitLab
        // single-selection guard pushes: anywhere in App.tsx, both strings
        // would still be found with the item moved out of that guard.
        const block = gitlabMenuBlock(appSource);
        expect(block.match(/items\.push\(/g) ?? []).toHaveLength(1);
        expect(block).toContain("label: t('gitlab.viewOnGitlab')");
        expect(block).toContain('openOnGitLab(invoke, openUrl, file.path, file.is_dir)');
    });

    it('opens external pages through openUrl, never window.open', () => {
        // window.open does not reach the browser from the Tauri 2 webview
        // (see utils/openUrl.ts): View on GitHub and File History used it.
        expect(appSource).not.toMatch(/window\.open\(/);
    });
});
