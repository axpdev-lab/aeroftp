/**
 * The source path a remote Paste hands to `provider_server_copy`.
 *
 * On S3 a folder and an object can share a name (`docs` and `docs/...`), and
 * the backend copies the object when the path has no trailing slash. A folder
 * row is sent with its slash, which on S3 names the folder (#1633). Inside a
 * crypt overlay the path is mapped by the overlay, so it is left as it is.
 */
export function serverCopySource(
  path: string,
  isDir: boolean,
  protocol: string | null | undefined,
  cryptOverlayActive: boolean,
): string {
  if (!isDir || protocol !== 's3' || cryptOverlayActive) return path;
  const trimmed = path.replace(/\/+$/, '');
  return trimmed === '' ? path : `${trimmed}/`;
}
