// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * Open the GitLab page of a file or folder in the browser. The backend builds
 * the URL (`gitlab_get_web_url`): it knows the instance base for self-hosted
 * GitLab, the working branch, and encodes branch and path segments.
 *
 * Rejects when the opener reports that nothing was opened (openUrl resolves
 * false instead of rejecting), so the caller's error handler still shows the
 * failure. The message carries no URL.
 */
export async function openOnGitLab(
    invoke: Invoke,
    open: (url: string) => Promise<unknown>,
    path: string,
    isDir: boolean,
): Promise<void> {
    const url = await invoke<string>('gitlab_get_web_url', { path, isDir });
    if ((await open(url)) === false) {
        throw new Error('Could not open the GitLab page in the browser');
    }
}

/**
 * Whether a GitLab server field names a self-hosted instance, by the rule the
 * backend uses (`instance_authority` in `providers/gitlab/mod.rs`): its first
 * segment holds a dot, is `localhost`, carries a port or is an IP address,
 * and is not gitlab.com. `owner/repo` is a project on gitlab.com. The
 * self-signed certificate option is offered only for such an instance.
 */
export function namesSelfHostedGitLab(server: string): boolean {
    const segment = server.trim().replace(/^https?:\/\//i, '').split('/')[0].toLowerCase();
    const marked = /[.:[]/.test(segment) || segment === 'localhost';
    if (!marked) return false;
    const host = segment.replace(/:443$/, '');
    return host !== 'gitlab.com';
}
