/**
 * Detect and localize the backend's restricted-character error.
 *
 * The Rust side (src-tauri/src/restricted_chars.rs) rejects file/folder names
 * containing characters a provider's storage backend forbids, returning a
 * `ProviderError::RestrictedChar` whose Display has a stable shape:
 *
 *   Restricted character "X" is not allowed by Jottacloud
 *   Restricted character U+0009 is not allowed by OpenDrive
 *
 * That string reaches the UI raw (possibly with a `Failed to rename: ` prefix).
 * Here we detect it and render a translated message naming the offending
 * character and provider, so the user understands why the operation failed
 * instead of seeing a silent or opaque error (discussion #272).
 */

// Char token is whitespace-free in both forms (`"X"` and `U+00NN`), and the
// provider label is the trailing token, so \S+ captures each cleanly.
const RESTRICTED_CHAR_RE = /Restricted character (\S+) is not allowed by (\S+)/;

type Translate = (key: string, opts?: Record<string, string | number>) => string;

/**
 * If `error` is a restricted-character error, return the localized message;
 * otherwise return null so the caller can fall back to the raw error string.
 */
export function localizeRestrictedCharError(error: unknown, t: Translate): string | null {
  const match = String(error).match(RESTRICTED_CHAR_RE);
  if (!match) return null;
  return t('error.restrictedChar', { char: match[1], provider: match[2] });
}
