// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderOptions } from '../types';

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
 * The instance a GitLab server field sends its token to, by the rule the
 * backend uses (`instance_authority` in `providers/gitlab/mod.rs`): a first
 * segment that holds a dot, a colon or a bracket, or is `localhost`, is the
 * instance (`host[:port]`, lowercased, 443 dropped); any other first segment
 * is a project owner, so the instance is gitlab.com. `null` when the field
 * names no address the backend accepts, which it then refuses. A username
 * that holds a dot (`john.doe/repo`) reads as the instance `john.doe`: the
 * form shows the result so that case is seen before the token leaves.
 */
export function gitlabInstanceOf(server: string): string | null {
    const segment = server.trim().replace(/^https?:\/\//i, '').split('/')[0];
    if (!segment) return null;
    const marked = /[.:[]/.test(segment) || segment.toLowerCase() === 'localhost';
    if (!marked) return 'gitlab.com';
    if (/[@?#\\]/.test(segment)) return null;
    try {
        const url = new URL(`https://${segment}/`);
        return url.host || null;
    } catch {
        return null;
    }
}

/**
 * Whether a GitLab server field names a self-hosted instance (see
 * `gitlabInstanceOf`). The self-signed certificate option is offered only
 * for such an instance.
 */
export function namesSelfHostedGitLab(server: string): boolean {
    const instance = gitlabInstanceOf(server);
    return instance !== null && instance !== 'gitlab.com';
}

/**
 * The form options for a newly typed server field. The self-signed
 * certificate opt-out is bound to the instance it was chosen for: when the
 * new server no longer names a self-hosted instance (a project on
 * gitlab.com, or a refused field), `verifyCert: false` is dropped, so the
 * token is never sent to gitlab.com with certificate validation off.
 */
export function gitlabOptionsForServer(
    options: ProviderOptions | undefined,
    server: string,
): ProviderOptions | undefined {
    if (options?.verifyCert === false && !namesSelfHostedGitLab(server)) {
        const { verifyCert: _dropped, ...rest } = options;
        return rest;
    }
    return options;
}
