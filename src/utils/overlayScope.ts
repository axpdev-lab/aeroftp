// Normalize an absolute remote path: trim, collapse duplicate slashes, strip the
// trailing slash; '' or '/' both mean the remote root.
export const normalizeRemotePath = (p: string): string => {
    const s = (p || '').trim().replace(/\/+/g, '/');
    if (s === '' || s === '/') return '';
    return (s.startsWith('/') ? s : '/' + s).replace(/\/$/, '');
};

// The overlays scope must equal the profile Remote Path or be a strict descendant.
// Ancestors, siblings and unrelated paths are invalid. A blank field = valid (it
// means "same as Remote Path"). "/" is a real value (the remote root): when the
// Remote Path is a subfolder, "/" is an ancestor and must be rejected (pinning the
// anchor above the vault is the misconfiguration that produces the empty-listing).
export const isValidOverlayScope = (scope: string, remotePath: string): boolean => {
    if ((scope || '').trim() === '') return true;
    const r = normalizeRemotePath(remotePath);
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
export const resolveOverlayScope = (input: string, remotePath: string): string => {
    const r = normalizeRemotePath(remotePath);
    const raw = (input || '').trim();
    if (raw === '' || raw === '/') return r;           // same as the Remote Path
    if (r === '') return normalizeRemotePath(raw);     // remote is root: any folder is absolute
    const s = normalizeRemotePath(raw);
    if (s === r || s.startsWith(r + '/')) return s;    // already absolute and in scope
    const rel = raw.replace(/^\/+/, '');               // strip leading slashes -> subfolder name
    return normalizeRemotePath(r + '/' + rel);         // nest under the Remote Path
};
