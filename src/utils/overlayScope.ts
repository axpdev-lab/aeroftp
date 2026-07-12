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
