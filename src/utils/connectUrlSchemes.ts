// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The URL schemes the File > Connect to URL... modal suggests, mirroring the
 * one table the CLI and the backend read (`src-tauri/src/launch_url.rs`,
 * `LAUNCH_SCHEMES`; aliases like `ssh://` or `b2://` are accepted by the
 * parser but not listed here). Templates mark their fill-in parts with
 * `{name}`: Tab inserts the template and selects the first placeholder, the
 * next Tab jumps to the following one, so a URL is typed without the mouse.
 */

export interface UrlSchemeSuggestion {
    /** What the URL starts with, lowercase, without `://`. */
    scheme: string;
    /** Short label shown next to the scheme. */
    label: string;
    /** Inserted on Tab; `{name}` marks a placeholder selected in turn. */
    template: string;
    /** Account/sign-in scheme: the app opens the matching saved profile. */
    signIn?: boolean;
    /** Extra prefixes the filter matches, e.g. `gdrive` for `googledrive`. */
    aliases?: string[];
}

const serverTemplate = (scheme: string) => `${scheme}://{user}@{host}/{path}`;
const accountTemplate = (scheme: string) => `${scheme}://{account}`;

export const URL_SCHEME_SUGGESTIONS: UrlSchemeSuggestion[] = [
    { scheme: 'sftp', label: 'SFTP server', template: serverTemplate('sftp'), aliases: ['ssh'] },
    { scheme: 'ftp', label: 'FTP server', template: serverTemplate('ftp') },
    { scheme: 'ftps', label: 'FTPS server', template: serverTemplate('ftps') },
    { scheme: 'scp', label: 'SCP', template: serverTemplate('scp') },
    { scheme: 'webdav', label: 'WebDAV', template: 'webdav://{user}@{host}/dav' },
    { scheme: 'webdavs', label: 'WebDAVs', template: 'webdavs://{user}@{host}/dav' },
    { scheme: 's3', label: 'S3 bucket', template: 's3://{accessKey}@{bucket}?region={region}' },
    { scheme: 'mega', label: 'MEGA', template: accountTemplate('mega') },
    { scheme: 'proton', label: 'Proton Drive', template: accountTemplate('proton'), aliases: ['protondrive'] },
    { scheme: 'azure', label: 'Azure Blob', template: serverTemplate('azure') },
    { scheme: 'filen', label: 'Filen', template: accountTemplate('filen') },
    { scheme: 'internxt', label: 'Internxt', template: accountTemplate('internxt') },
    { scheme: 'jottacloud', label: 'Jottacloud', template: accountTemplate('jottacloud') },
    { scheme: 'filelu', label: 'FileLu', template: accountTemplate('filelu') },
    { scheme: 'koofr', label: 'Koofr', template: accountTemplate('koofr') },
    { scheme: 'opendrive', label: 'OpenDrive', template: accountTemplate('opendrive') },
    { scheme: 'github', label: 'GitHub repository', template: 'github://{owner}/{repo}' },
    { scheme: 'gitlab', label: 'GitLab project', template: 'gitlab://{owner}/{repo}' },
    { scheme: 'kdrive', label: 'kDrive', template: accountTemplate('kdrive') },
    { scheme: 'drime', label: 'Drime', template: accountTemplate('drime') },
    { scheme: 'swift', label: 'Swift (OpenStack)', template: 'swift://{user}@{host}/{container}' },
    { scheme: 'immich', label: 'Immich', template: accountTemplate('immich') },
    { scheme: 'imagekit', label: 'ImageKit', template: accountTemplate('imagekit') },
    { scheme: 'uploadcare', label: 'Uploadcare', template: accountTemplate('uploadcare') },
    { scheme: 'backblaze', label: 'Backblaze B2', template: accountTemplate('backblaze'), aliases: ['b2'] },
    { scheme: 'cloudinary', label: 'Cloudinary', template: accountTemplate('cloudinary') },
    { scheme: 'yandexdisk', label: 'Yandex Disk', template: accountTemplate('yandexdisk'), signIn: true },
    { scheme: 'googledrive', label: 'Google Drive', template: accountTemplate('googledrive'), signIn: true, aliases: ['gdrive', 'drive'] },
    { scheme: 'dropbox', label: 'Dropbox', template: accountTemplate('dropbox'), signIn: true },
    { scheme: 'onedrive', label: 'OneDrive', template: accountTemplate('onedrive'), signIn: true },
    { scheme: 'box', label: 'Box', template: accountTemplate('box'), signIn: true },
    { scheme: 'pcloud', label: 'pCloud', template: accountTemplate('pcloud'), signIn: true },
    { scheme: 'zohoworkdrive', label: 'Zoho WorkDrive', template: accountTemplate('zohoworkdrive'), signIn: true },
    { scheme: 'fourshared', label: '4shared', template: accountTemplate('fourshared'), signIn: true },
    { scheme: 'twake', label: 'Twake Drive', template: accountTemplate('twake'), signIn: true },
];

export interface PlaceholderRange {
    start: number;
    end: number;
    name: string;
}

/** The `{name}` parts of a template, in order, as offsets into it. */
export function placeholderRanges(template: string): PlaceholderRange[] {
    const ranges: PlaceholderRange[] = [];
    const re = /\{([^{}]+)\}/g;
    let match: RegExpExecArray | null;
    while ((match = re.exec(template)) !== null) {
        ranges.push({ start: match.index + 1, end: match.index + 1 + match[1].length, name: match[1] });
    }
    return ranges;
}

/** The scheme a typed value starts with, lowercase, or null. */
export function schemeOfValue(value: string): string | null {
    const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\//.exec(value.trim());
    return match ? match[1].toLowerCase() : null;
}

/** The suggestion a typed value's scheme (or alias, like `gdrive`) names. */
export function suggestionForValue(value: string): UrlSchemeSuggestion | null {
    const scheme = schemeOfValue(value);
    if (!scheme) return null;
    return URL_SCHEME_SUGGESTIONS.find(
        (s) => s.scheme === scheme || (s.aliases ?? []).includes(scheme),
    ) ?? null;
}

/** Whether the URL carries an inline `user:password@` credential. */
export function hasInlinePassword(value: string): boolean {
    return /^[a-zA-Z][a-zA-Z0-9+.-]*:\/\/[^/@:\s]+:[^@\s]*@/i.test(value.trim());
}

/**
 * The profile name a URL suggests for "Save to My Servers": `user@host`,
 * else the host, else the account; for S3 the bucket (the host part).
 */
export function suggestProfileName(value: string): string {
    const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):\/\/([^/?#]*)/.exec(value.trim());
    if (!match) return '';
    const scheme = match[1].toLowerCase();
    const authority = match[2];
    const at = authority.lastIndexOf('@');
    const user = at >= 0 ? authority.slice(0, at) : '';
    const host = (at >= 0 ? authority.slice(at + 1) : authority).split(':')[0];
    if (scheme === 's3') return host || user;
    if (user && host) return `${user}@${host}`;
    return host || user;
}

/**
 * The suggestions for what is typed so far, at most `max`. A scheme prefix
 * wins over a label match; with an empty query the first `max` entries show.
 */
export function filterSuggestions(query: string, max = 6): UrlSchemeSuggestion[] {
    const q = query.toLowerCase().split('://')[0].trim();
    const scored: Array<{ suggestion: UrlSchemeSuggestion; score: number }> = [];
    for (const suggestion of URL_SCHEME_SUGGESTIONS) {
        if (!q) {
            scored.push({ suggestion, score: 0 });
            continue;
        }
        const scheme = suggestion.scheme.toLowerCase();
        const aliasHit = (suggestion.aliases ?? []).some((a) => a.startsWith(q));
        if (scheme.startsWith(q) || aliasHit) scored.push({ suggestion, score: 0 });
        else if (scheme.includes(q) || suggestion.label.toLowerCase().includes(q)) scored.push({ suggestion, score: 1 });
        else if (suggestion.template.toLowerCase().includes(q)) scored.push({ suggestion, score: 2 });
    }
    scored.sort((a, b) => a.score - b.score);
    return scored.slice(0, max).map((entry) => entry.suggestion);
}

/** Tab on a fresh field: the template as the new value, first placeholder selected. */
export function applyTemplate(template: string): { value: string; selection: PlaceholderRange | null } {
    return { value: template, selection: placeholderRanges(template)[0] ?? null };
}

/** Tab on a template already inserted: the placeholder after the caret, if any. */
export function nextPlaceholder(template: string, caret: number): PlaceholderRange | null {
    return placeholderRanges(template).find((range) => range.start > caret) ?? null;
}
