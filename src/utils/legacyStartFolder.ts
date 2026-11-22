/**
 * The start folder a saved profile really opens, for profiles saved while a
 * preset still repeated part of its server URL as a `basePath`. The provider
 * joins the start folder onto the URL path, so such a profile opened one level
 * too deep. Mirrors `strip_legacy_start_folder` in
 * `src-tauri/src/profile_loader.rs`, which the CLI and MCP use.
 *
 * Returns `null` for the root, so the WebDAV auto-detect drives as it does for
 * a freshly saved profile. Any other start folder is kept as written.
 */

/** Hosted Nextcloud presets whose WebDAV root profiles saved as start folder. */
const NEXTCLOUD_ROOT_PRESETS: ReadonlySet<string> = new Set([
    'tabdigital',
    'tabdigital-webdav',
    'felicloud',
    'felicloud-webdav',
    'murena',
    'nextcloud',
]);

/**
 * Presets whose old `basePath` was the last path of their own server URL.
 * Jianguoyun carried `/dav/` over `https://dav.jianguoyun.com/dav` until
 * 2026-11-22.
 */
const REPEATED_BASE_PATHS: Readonly<Record<string, string>> = {
    jianguoyun: '/dav',
};

/** The path of a server URL, with or without a scheme, without trailing slashes. */
const serverUrlPath = (server: string): string => {
    // A slash inside the query or fragment is not the start of the path.
    const beforeQuery = server.trim().replace(/^[a-z][a-z0-9+.-]*:\/\//i, '').split(/[?#]/)[0];
    const slash = beforeQuery.indexOf('/');
    if (slash < 0) return '';
    return beforeQuery.slice(slash).replace(/\/+$/, '');
};

export const stripLegacyStartFolder = (
    initialPath: string | null | undefined,
    providerId: string | null | undefined,
    username: string | null | undefined,
    server: string | null | undefined,
): string | null => {
    if (!initialPath) return initialPath ?? null;
    const pid = providerId || '';
    const trimmed = initialPath.replace(/\/+$/, '');

    const repeated = REPEATED_BASE_PATHS[pid];
    if (repeated && trimmed === repeated && serverUrlPath(server || '').endsWith(repeated)) {
        return null;
    }

    if (NEXTCLOUD_ROOT_PRESETS.has(pid)) {
        // Blank is read trimmed; the path is compared with the username as stored.
        const user = username || '';
        if (user.trim() && trimmed === `/remote.php/dav/files/${user}`) return null;
    }
    return initialPath;
};
