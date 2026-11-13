const normalizeSegments = (value: string): string[] => {
    const segments: string[] = [];
    for (const part of value.split('/')) {
        if (!part || part === '.') continue;
        if (part === '..') {
            segments.pop();
            continue;
        }
        segments.push(part);
    }
    return segments;
};

// Normalize an absolute remote path: preserve name bytes, collapse duplicate slashes, resolve
// dot segments, strip the trailing slash; '' or '/' both mean the remote root.
export const normalizeRemotePath = (p: string): string => {
    const s = p || '';
    if (s === '' || s === '/') return '';
    const normalized = normalizeSegments(s).join('/');
    return normalized ? '/' + normalized : '';
};

/**
 * A folder under the login folder (`~`, `~/docs`), as the rclone import writes
 * for an SFTP crypt remote such as `nas:docs`. Only the connected session
 * knows the absolute folder it names: the backend resolves it when it applies
 * the overlay and returns that anchor. Read as a path before then, `~/docs`
 * became `/~/docs`, a folder nobody wrote to.
 */
export const isLoginRelativeScope = (s?: string | null): boolean =>
    s === '~' || !!s?.startsWith('~/');

/**
 * Protocols that open a relative folder under the login folder: SFTP and SCP
 * resolve it against home (`resolve_start_dir` in the backend), FTP and FTPS
 * issue a relative `CWD` from the login directory. There `docs` is `~/docs`;
 * read from the root it was `/docs`, a folder the session never opened.
 */
export const loginRelativeProtocol = (protocol?: string | null): boolean =>
    protocol === 'sftp' || protocol === 'scp' || protocol === 'ftp' || protocol === 'ftps';

/**
 * A path as the protocol opens it. On a login-relative protocol a relative
 * folder opens under the login folder, so there `docs` is `~/docs`; read from
 * the root it was `/docs`, which the session never opened. Any other path,
 * and any other protocol, comes back as given. The backend maps a relative
 * scope the same way (`binding_with_resolved_scope`).
 */
export const loginRelativeForm = (path: string | null | undefined, protocol?: string | null): string => {
    const p = path || '';
    if (!p || p.startsWith('/') || isLoginRelativeScope(p) || !loginRelativeProtocol(protocol)) return p;
    return `~/${p}`;
};

/** An overlay anchor in the form the backend reads: `~/...` normalized as a login-relative path, anything else as an absolute one. */
export const normalizeOverlayAnchor = (p: string | null | undefined): string =>
    isLoginRelativeScope(p) ? normalizeLoginRelative(p as string) : normalizeRemotePath(p || '');

/**
 * `~` and `~/...` with `.` and empty segments dropped and no trailing slash,
 * as the backend places it. A `..` is left as written: it would leave the
 * login folder, and the backend refuses it rather than anchor there.
 */
export const normalizeLoginRelative = (p: string): string => {
    if (p === '~') return p;
    const segments = p.slice(2).split('/');
    if (segments.includes('..')) return p;
    const rest = segments.filter((segment) => segment !== '' && segment !== '.').join('/');
    return rest ? `~/${rest}` : '~';
};

// The overlays scope must equal the profile Remote Path or be a strict descendant.
// Ancestors, siblings and unrelated paths are invalid. A blank field = valid (it
// means "same as Remote Path"). "/" is a real value (the remote root): when the
// Remote Path is a subfolder, "/" is an ancestor and must be rejected (pinning the
// anchor above the vault is the misconfiguration that produces the empty-listing).
export const isValidOverlayScope = (scope: string, remotePath: string, protocol?: string | null): boolean => {
    if (!scope) return true;
    // A `..` would leave the login folder: the backend refuses it, so the form does too.
    if (isLoginRelativeScope(scope) && normalizeLoginRelative(scope).split('/').includes('..')) return false;
    const remote = loginRelativeForm(remotePath, protocol);
    if (isLoginRelativeScope(remote)) {
        // Under the login folder only a scope in the same form compares.
        if (!isLoginRelativeScope(scope)) return false;
        const r = normalizeLoginRelative(remote);
        const s = normalizeLoginRelative(scope);
        return s === r || s.startsWith(r + '/');
    }
    const r = normalizeRemotePath(remote);
    if (r === '') return true;            // remote path is root: any folder is a descendant
    const s = normalizeRemotePath(scope);
    if (s === '') return false;           // scope is "/" but remote is a subfolder: ancestor
    return s === r || s.startsWith(r + '/');
};

// Resolve the operator's overlays-scope input into the absolute path we persist,
// interpreting it RELATIVE to the Remote Path so the prefix is never re-typed
// (#369 UX). Blank or "/" means "the Remote Path itself". A value already equal
// to or under the Remote Path is kept verbatim (an operator may still paste the
// full absolute path). Anything else is treated as a subfolder name and nested
// under the Remote Path, so the anchor can never escape the Remote Path by
// construction and isValidOverlayScope(resolveOverlayScope(x, r), r) is always
// true. The returned value is normalized (the exact form the backend reads).
export const resolveOverlayScope = (input: string, remotePath: string, protocol?: string | null): string => {
    const raw = input || '';
    const remote = loginRelativeForm(remotePath, protocol);
    if (isLoginRelativeScope(remote)) {
        // Only the session places the login folder, so the anchor stays in
        // that form: written `/~/docs`, it named a folder nobody wrote to.
        const r = normalizeLoginRelative(remote);
        if (raw === '' || raw === '/') return r;
        if (isLoginRelativeScope(raw)) {
            const s = normalizeLoginRelative(raw);
            if (s === r || s.startsWith(r + '/')) return s;
        }
        // An absolute folder cannot be compared with one the session has not
        // placed yet: kept as typed, and `isValidOverlayScope` refuses it.
        if (raw.startsWith('/')) return normalizeRemotePath(raw);
        const rel = normalizeSegments(raw.replace(/^~(?=\/|$)/, '').replace(/^\/+/, '')).join('/');
        return rel ? `${r}/${rel}` : r;
    }
    const r = normalizeRemotePath(remote);
    if (raw === '' || raw === '/') return r;           // same as the Remote Path
    if (r === '') {                                    // remote is root: any folder is absolute
        return loginRelativeProtocol(protocol) && isLoginRelativeScope(raw) ? normalizeLoginRelative(raw) : normalizeRemotePath(raw);
    }
    const s = normalizeRemotePath(raw);
    if (s === r || s.startsWith(r + '/')) return s;    // already absolute and in scope
    const rel = normalizeSegments(raw.replace(/^\/+/, '')).join('/');
    return normalizeRemotePath(r + '/' + rel);         // nest under the Remote Path
};
