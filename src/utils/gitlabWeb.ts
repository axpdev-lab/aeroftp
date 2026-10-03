// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

type Invoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * Open the GitLab page of a file or folder in the browser. The backend builds
 * the URL (`gitlab_get_web_url`): it knows the instance base for self-hosted
 * GitLab, the working branch, and encodes branch and path segments.
 */
export async function openOnGitLab(
    invoke: Invoke,
    open: (url: string) => Promise<void>,
    path: string,
    isDir: boolean,
): Promise<void> {
    const url = await invoke<string>('gitlab_get_web_url', { path, isDir });
    await open(url);
}
