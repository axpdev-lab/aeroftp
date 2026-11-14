// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The `{username}` placeholder some WebDAV presets keep in their starting
 * folder (Nextcloud and its hosted relatives). This is the only place the
 * frontend fills it in, mirroring the backend's
 * `connection_manifest::expand_username_in_path`; a source guard test fails
 * if another file does it.
 */
import cases from '../shared/username-template-cases.json';

export const USERNAME_TEMPLATE = '{username}';

/** What the window shows instead of a location still holding the placeholder;
 *  the same text as the backend's `USERNAME_TEMPLATE_UNRESOLVED`. */
export const USERNAME_TEMPLATE_UNRESOLVED: string = cases.unresolvedMessage;

/** True when a derived location still holds the placeholder: the user name
 *  could not fill it, so nothing may be saved, enabled or synced there. */
export function hasUnresolvedUsernameTemplate(location: string | null | undefined): boolean {
    return !!location && location.includes(USERNAME_TEMPLATE);
}

/**
 * The folder path with `{username}` filled by the user name as written. A
 * blank user name, or one that could move the folder (`/`, `\`, NUL, `.` or
 * `..`), leaves the template as it is: never a shorter path, so a derived
 * scope or root can never climb above the user's folder. The connect then
 * refuses such a profile before any request is made.
 *
 * Nothing is trimmed, before or after, as in the backend: a folder's edge
 * spaces are part of its name (#1449), as are the user name's, so the
 * AeroCloud root and the crypt overlay anchor name one folder. The shared
 * table's `compositions` cases fail if the helper or a reader trims.
 */
export function expandUsernameInPath(template: string, username: string | null | undefined): string {
    if (!template.includes(USERNAME_TEMPLATE)) return template;
    // Inserted exactly as stored, never trimmed (the login sends it that
    // way); blankness and `.`/`..` are judged on the trimmed value.
    const u = username || '';
    const t = u.trim();
    if (!t || /[/\\\0]/.test(u) || t === '.' || t === '..' || u.includes(USERNAME_TEMPLATE)) return template;
    // split/join, never String.replace: a `$&` in a user name is no pattern.
    return template.split(USERNAME_TEMPLATE).join(u);
}
