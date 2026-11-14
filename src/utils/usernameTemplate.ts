// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The `{username}` placeholder some WebDAV presets keep in their starting
 * folder (Nextcloud and its hosted relatives). This is the only place the
 * frontend fills it in, mirroring the backend's
 * `connection_manifest::expand_username_in_path`; a source guard test fails
 * if another file does it.
 */
export const USERNAME_TEMPLATE = '{username}';

/**
 * The folder path with `{username}` filled by the user name as written. A
 * blank user name, or one that could move the folder (`/`, `\`, NUL, `.` or
 * `..`), leaves the template as it is: never a shorter path, so a derived
 * scope or root can never climb above the user's folder. The connect then
 * refuses such a profile before any request is made.
 */
export function expandUsernameInPath(template: string, username: string | null | undefined): string {
    if (!template.includes(USERNAME_TEMPLATE)) return template;
    // Inserted exactly as stored, never trimmed (the login sends it that
    // way); blankness and `.`/`..` are judged on the trimmed value.
    const u = username || '';
    const t = u.trim();
    if (!t || /[/\\\0]/.test(u) || t === '.' || t === '..') return template;
    // split/join, never String.replace: a `$&` in a user name is no pattern.
    return template.split(USERNAME_TEMPLATE).join(u);
}
