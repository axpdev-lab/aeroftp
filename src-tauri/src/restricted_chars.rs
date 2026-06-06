//! Per-provider restricted filename character detection.
//!
//! Several cloud backends forbid characters in file and folder names that are
//! perfectly legal on the local filesystem (`:`, `?`, `\`, `*`, ...). When such
//! a name reaches the provider API unmodified the operation fails silently or
//! with an opaque server error (the hunt in discussion #272). This module
//! detects the offending character at the shared command and transfer
//! chokepoints, BEFORE the name is sent, so the user gets a clear, actionable
//! message instead.
//!
//! The forbidden-character tables mirror rclone's published restricted-character
//! sets so behaviour matches a well-known reference:
//! - <https://rclone.org/overview/#restricted-characters> (universal set)
//! - <https://rclone.org/box/#restricted-filename-characters>
//! - <https://rclone.org/dropbox/#restricted-filename-characters>
//! - <https://rclone.org/jottacloud/#restricted-filename-characters>
//! - <https://rclone.org/opendrive/#restricted-filename-characters>
//!
//! This is Phase 1 (detect + clear error). Phase 2 (reversible rclone-style
//! encode/decode so the names round-trip) builds on the same tables.

use crate::providers::types::{ProviderError, ProviderType};

/// Human-readable rendering of the offending character for the error message:
/// printable characters are quoted (`":"`), control characters are shown by
/// code point (`U+0009`) since they would be invisible otherwise.
fn display_char(c: char) -> String {
    if c.is_control() {
        format!("U+{:04X}", c as u32)
    } else {
        format!("\"{}\"", c)
    }
}

/// Human label for the provider, used both in the error message and as the
/// token the GUI extracts to localize it. `Debug` already yields the catalog
/// names (`Box`, `Dropbox`, `Jottacloud`, `OpenDrive`, ...).
fn provider_label(provider: ProviderType) -> String {
    format!("{:?}", provider)
}

/// Last non-empty path segment, i.e. the file or folder name being created or
/// renamed. Tolerates leading/trailing/duplicate slashes.
fn leaf_of(path: &str) -> &str {
    path.rsplit('/').find(|s| !s.is_empty()).unwrap_or("")
}

/// Validate a single name leaf (no path separators expected).
///
/// Returns [`ProviderError::RestrictedChar`] naming the first offending
/// character. The wording is stable so the GUI can detect and localize it.
pub fn validate_filename(provider: ProviderType, leaf: &str) -> Result<(), ProviderError> {
    // Providers with a Phase 2 encoding spec never hard-fail: any character
    // they would otherwise reject is instead stored in its reversible encoded
    // form (see [`encode_leaf`]). The hard error below is therefore only for the
    // control-characters-only providers, which have no reversible encoding.
    if encode_spec(provider).is_some() {
        return Ok(());
    }
    // Control-characters-only providers: ASCII/Unicode control characters (C0,
    // DEL, C1) break virtually every storage API and never appear in a
    // legitimate name. Path separators are not checked here: callers validate a
    // single leaf.
    for c in leaf.chars() {
        if c.is_control() {
            return Err(ProviderError::RestrictedChar {
                ch_display: display_char(c),
                provider: provider_label(provider),
            });
        }
    }
    Ok(())
}

/// Validate the leaf of a full remote path (upload target, rename `to`, mkdir
/// path). Only the final segment is checked: intermediate components were
/// validated when they were originally created.
pub fn validate_path(provider: ProviderType, path: &str) -> Result<(), ProviderError> {
    validate_filename(provider, leaf_of(path))
}

// ---------------------------------------------------------------------------
// Phase 2: reversible, rclone-compatible encoding
// ---------------------------------------------------------------------------
//
// Phase 1 (above) DETECTS a forbidden character and returns a clear error.
// Phase 2 instead ENCODES forbidden characters to reversible Unicode
// replacements so a name a provider's API rejects can still be stored and
// round-trips losslessly:
//   - encode a name on its way TO the provider API (upload, mkdir, rename, ...),
//   - decode names coming FROM the provider (list, stat, pwd, ...) for display.
// The user always types/sees the ORIGINAL name; the provider stores the
// ENCODED form; neither side sees the other's.
//
// The scheme is a faithful port of rclone's `lib/encoder` so names interoperate
// with rclone-written data (same fullwidth/control-picture mappings, same
// `QuoteRune` escape for collisions):
//   - printable ASCII punctuation -> fullwidth form (`c as u32 + 0xFEE0`),
//   - C0 control + NUL -> Control Pictures block (`code + 0x2400`),
//   - DEL (0x7F) -> U+2421, SPACE (position-dependent) -> U+2420,
//   - whole-name `.`/`..` -> fullwidth `．`/`．．`,
//   - any LITERAL occurrence of a replacement rune (or the quote rune itself) is
//     escaped with `QUOTE` so decoding is unambiguous (the collision case).
//
// Only Box, Dropbox, Jottacloud and OpenDrive forbid printable characters and
// therefore need encoding; every other provider is control-characters-only and
// keeps Phase 1's hard error. The per-provider punctuation tables live in
// [`encode_spec`] below (the owner-verified rclone source of truth); having a
// spec is also what makes [`validate_filename`] skip the hard error.
//
// Scope note (intentional, documented): rclone's Box/Jottacloud defaults encode
// control characters everywhere (`Display`), while its Dropbox/OpenDrive
// defaults (`Base`) encode control characters only at the name edges. We encode
// C0/DEL everywhere for ALL FOUR providers, because Phase 1 already declared
// control characters universally name-breaking on these APIs; encoding them is
// reversible and strictly safer than letting a raw control byte reach the API.
// This makes rclone's separate left/right CR-LF-HT-VT edge rules redundant, so
// the only position-dependent rule we keep is the trailing (Box, Dropbox) or
// leading+trailing (OpenDrive) SPACE rule.

/// U+201B SINGLE HIGH-REVERSED-9 QUOTATION MARK. rclone's `QuoteRune`: prefixed
/// before a literal occurrence of any replacement rune (or before itself) so the
/// decoder can tell "this rune was produced by encoding" from "this rune was
/// already in the user's name".
const QUOTE: char = '\u{201B}';
/// Offset mapping printable ASCII (U+0021..U+007E) to its fullwidth twin
/// (U+FF01..U+FF5E). `':'` (U+003A) -> `'：'` (U+FF1A), etc.
const FULLWIDTH_OFFSET: u32 = 0xFEE0;
/// Start of the Control Pictures block: C0 control `code` -> `code + 0x2400`
/// (NUL -> U+2400 `␀`, TAB -> U+2409 `␉`, ...).
const CONTROL_PICTURE_OFFSET: u32 = 0x2400;
/// U+2421 SYMBOL FOR DELETE, the replacement for DEL (0x7F).
const SYMBOL_DEL: char = '\u{2421}';
/// U+2420 SYMBOL FOR SPACE, the replacement for a leading/trailing space.
const SYMBOL_SPACE: char = '\u{2420}';
/// U+FF0E FULLWIDTH FULL STOP, the replacement rune for whole-name `.`/`..`.
const FULLWIDTH_DOT: char = '\u{FF0E}';

/// Per-provider encoding contract. `chars` lists the printable punctuation that
/// provider forbids (fullwidth-mapped anywhere in a leaf). Control characters
/// and whole-name dots are encoded for every provider that has a spec; the only
/// per-provider variation beyond `chars` is which name edges restrict a space.
struct EncodeSpec {
    /// Printable ASCII punctuation forbidden anywhere in a name leaf.
    chars: &'static [char],
    /// Restrict a leading space (encode it to `␠`). OpenDrive only.
    left_space: bool,
    /// Restrict a trailing space (encode it to `␠`). Box, Dropbox, OpenDrive.
    right_space: bool,
}

/// The encoding contract for `provider`, or `None` for the control-chars-only
/// providers that need no reversible encoding (they keep Phase 1's hard error).
/// The `chars` tables are the owner-verified rclone per-backend tables.
fn encode_spec(provider: ProviderType) -> Option<EncodeSpec> {
    match provider {
        ProviderType::Box => Some(EncodeSpec {
            chars: &['\\'],
            left_space: false,
            right_space: true,
        }),
        ProviderType::Dropbox => Some(EncodeSpec {
            chars: &['\\'],
            left_space: false,
            right_space: true,
        }),
        ProviderType::Jottacloud => Some(EncodeSpec {
            chars: &['"', '*', ':', '<', '>', '?', '|', '%'],
            left_space: false,
            right_space: false,
        }),
        ProviderType::OpenDrive => Some(EncodeSpec {
            chars: &['"', '*', ':', '<', '>', '?', '\\', '|'],
            left_space: true,
            right_space: true,
        }),
        _ => None,
    }
}

/// If `c` is a source character this spec encodes anywhere in a leaf, return its
/// replacement rune. Covers C0 control (incl. NUL) + DEL universally, plus the
/// provider's punctuation table. Space is NOT here: it is position-dependent and
/// handled by the prefix/suffix pass, not the main loop.
fn encode_source(spec: &EncodeSpec, c: char) -> Option<char> {
    if c == '\u{7F}' {
        return Some(SYMBOL_DEL);
    }
    if (c as u32) < 0x20 {
        // NUL..US -> Control Pictures (always valid: 0x2400..0x241F).
        return char::from_u32(c as u32 + CONTROL_PICTURE_OFFSET);
    }
    if spec.chars.contains(&c) {
        // Printable ASCII punctuation -> fullwidth twin.
        return char::from_u32(c as u32 + FULLWIDTH_OFFSET);
    }
    None
}

/// If `r` is the replacement rune for some source character this spec encodes,
/// return that original source character. Inverse of [`encode_source`]; also the
/// predicate for "this rune must be quote-escaped if it appears literally".
fn decode_target(spec: &EncodeSpec, r: char) -> Option<char> {
    if r == SYMBOL_DEL {
        return Some('\u{7F}');
    }
    let u = r as u32;
    if (CONTROL_PICTURE_OFFSET..CONTROL_PICTURE_OFFSET + 0x20).contains(&u) {
        // 0x2400..0x241F -> 0x00..0x1F.
        return char::from_u32(u - CONTROL_PICTURE_OFFSET);
    }
    if (0xFF01..=0xFF5E).contains(&u) {
        let src = char::from_u32(u - FULLWIDTH_OFFSET)?;
        if spec.chars.contains(&src) {
            return Some(src);
        }
    }
    None
}

/// True if `r` is a replacement rune that, appearing LITERALLY in the user's
/// name, must be quote-escaped so decoding is unambiguous. That is the quote
/// rune itself, the space symbol (when this spec restricts a space), and every
/// rune [`decode_target`] would otherwise turn back into a source character.
fn is_reserved(spec: &EncodeSpec, r: char) -> bool {
    r == QUOTE
        || (r == SYMBOL_SPACE && (spec.left_space || spec.right_space))
        || decode_target(spec, r).is_some()
}

/// Encode a single name leaf (no `/` separators) for `provider`. Returns the
/// name unchanged for providers without an [`encode_spec`].
pub fn encode_leaf(provider: ProviderType, leaf: &str) -> String {
    let Some(spec) = encode_spec(provider) else {
        return leaf.to_string();
    };
    // Whole-name dot cases (rclone short-circuits these before anything else).
    // A literal fullwidth-dot name is escaped so it cannot be read back as `.`.
    match leaf {
        "." => return FULLWIDTH_DOT.to_string(),
        ".." => return format!("{FULLWIDTH_DOT}{FULLWIDTH_DOT}"),
        "\u{FF0E}" => return format!("{QUOTE}{FULLWIDTH_DOT}"),
        "\u{FF0E}\u{FF0E}" => {
            return format!("{QUOTE}{FULLWIDTH_DOT}{QUOTE}{FULLWIDTH_DOT}");
        }
        _ => {}
    }

    let mut middle = leaf;
    let mut prefix = String::new();
    let mut suffix = String::new();

    // Position-dependent SPACE: only an ACTUAL leading/trailing space is lifted
    // out here; a literal `␠` is left in `middle` to be quote-escaped below.
    if spec.left_space && middle.starts_with(' ') {
        prefix.push(SYMBOL_SPACE);
        middle = &middle[1..];
    }
    if spec.right_space && middle.ends_with(' ') {
        suffix.push(SYMBOL_SPACE);
        middle = &middle[..middle.len() - 1];
    }

    let mut out = prefix;
    for c in middle.chars() {
        if c == QUOTE {
            // Escape a literal quote rune: `‛` -> `‛‛`.
            out.push(QUOTE);
            out.push(QUOTE);
        } else if let Some(repl) = encode_source(&spec, c) {
            out.push(repl);
        } else if is_reserved(&spec, c) {
            // A literal occurrence of a replacement rune: escape it.
            out.push(QUOTE);
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out.push_str(&suffix);
    out
}

/// Decode a single name leaf coming from `provider`. Inverse of [`encode_leaf`].
pub fn decode_leaf(provider: ProviderType, leaf: &str) -> String {
    let Some(spec) = encode_spec(provider) else {
        return leaf.to_string();
    };
    // Whole-name dot cases, mirroring the encoder's short-circuits.
    match leaf {
        "\u{FF0E}" => return ".".to_string(),
        "\u{FF0E}\u{FF0E}" => return "..".to_string(),
        _ => {}
    }

    let chars: Vec<char> = leaf.chars().collect();
    let n = chars.len();
    let mut start = 0;
    let mut out = String::new();

    // Reverse a position-dependent LEADING space. A leading `␠` is always a real
    // encoded leading space: a literal one would have arrived as `‛␠`, so the
    // first rune would be the quote, not `␠`. The TRAILING space is handled
    // inside the loop instead, because a look-behind cannot tell an escaping
    // quote (`‛␠`) from an escaped quote that merely precedes `␠` (`‛‛␠`); the
    // left-to-right quote state can.
    if spec.left_space && chars.first() == Some(&SYMBOL_SPACE) {
        out.push(' ');
        start += 1;
    }

    let mut i = start;
    while i < n {
        let c = chars[i];
        if c == QUOTE {
            // Next rune is a quoted literal (a replacement rune or the quote).
            if i + 1 < n {
                out.push(chars[i + 1]);
                i += 2;
            } else {
                out.push(QUOTE);
                i += 1;
            }
        } else if spec.right_space && c == SYMBOL_SPACE && i == n - 1 {
            // An UNquoted `␠` can only be the trailing-space marker: a literal
            // `␠` would have been quoted (`‛␠`) and consumed by the branch above.
            out.push(' ');
            i += 1;
        } else if let Some(src) = decode_target(&spec, c) {
            out.push(src);
            i += 1;
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// Encode every segment of a remote path for `provider`, leaving the `/`
/// separators (and empty segments from leading/trailing/duplicate slashes)
/// untouched so absolute paths and trailing slashes are preserved.
pub fn encode_path(provider: ProviderType, path: &str) -> String {
    encode_spec(provider)
        .map(|_| join_segments(path, |seg| encode_leaf(provider, seg)))
        .unwrap_or_else(|| path.to_string())
}

/// Decode every segment of a remote path coming from `provider`. Inverse of
/// [`encode_path`].
pub fn decode_path(provider: ProviderType, path: &str) -> String {
    encode_spec(provider)
        .map(|_| join_segments(path, |seg| decode_leaf(provider, seg)))
        .unwrap_or_else(|| path.to_string())
}

/// Apply `f` to each non-empty `/`-separated segment, preserving separators and
/// empty segments verbatim.
fn join_segments(path: &str, f: impl Fn(&str) -> String) -> String {
    path.split('/')
        .map(|seg| if seg.is_empty() { String::new() } else { f(seg) })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err_provider(provider: ProviderType, name: &str) -> Option<String> {
        match validate_filename(provider, name) {
            Err(ProviderError::RestrictedChar { provider, .. }) => Some(provider),
            Err(other) => panic!("unexpected error variant: {other:?}"),
            Ok(()) => None,
        }
    }

    #[test]
    fn plain_names_are_allowed_everywhere() {
        for p in [
            ProviderType::Box,
            ProviderType::Dropbox,
            ProviderType::Jottacloud,
            ProviderType::OpenDrive,
            ProviderType::S3,
            ProviderType::Ftp,
        ] {
            assert!(validate_filename(p, "report 2026 (final).pdf").is_ok());
            assert!(validate_filename(p, "résumé.txt").is_ok());
        }
    }

    #[test]
    fn control_characters_forbidden_only_on_non_encoding_providers() {
        // Control-chars-only providers still hard-fail (no reversible encoding).
        assert!(validate_filename(ProviderType::S3, "bad\tname").is_err());
        assert!(validate_filename(ProviderType::Ftp, "bad\nname").is_err());
        // Providers with a Phase 2 spec no longer hard-fail: the control
        // character is encoded instead (see Phase 2 round-trip tests).
        assert!(validate_filename(ProviderType::Box, "bad\u{0001}name").is_ok());
    }

    #[test]
    fn encoding_providers_no_longer_hard_fail() {
        // Every character a provider's table once rejected now validates OK,
        // because Phase 2 stores it in reversible encoded form instead.
        for c in ['"', '*', ':', '<', '>', '?', '\\', '|', '%'] {
            let name = format!("a{c}b");
            for p in ENCODING_PROVIDERS {
                assert!(
                    validate_filename(p, &name).is_ok(),
                    "expected {p:?} to accept {c:?} (encoded, not rejected)"
                );
            }
        }
    }

    #[test]
    fn validate_path_checks_only_the_leaf() {
        // On a control-chars-only provider, a control char in an already-existing
        // parent segment is not the operation's concern; only the new leaf is
        // validated.
        assert!(validate_path(ProviderType::S3, "/ok/parent/file.txt").is_ok());
        assert!(validate_path(ProviderType::S3, "/ok/parent/a\tb.txt").is_err());
        // Trailing slash (folder) still resolves to the leaf.
        assert!(validate_path(ProviderType::S3, "/ok/a\nb/").is_err());
    }

    #[test]
    fn error_message_names_char_and_provider() {
        // The remaining hard error is a control char on a non-encoding provider.
        let err = validate_filename(ProviderType::S3, "a\tb").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("S3"), "message was: {msg}");
        assert_eq!(err_provider(ProviderType::S3, "a\tb").as_deref(), Some("S3"));
    }

    #[test]
    fn control_char_renders_as_code_point() {
        let err = validate_filename(ProviderType::S3, "a\tb").unwrap_err();
        assert!(err.to_string().contains("U+0009"), "got: {err}");
    }

    // ----- Phase 2: reversible encoding -----

    const ENCODING_PROVIDERS: [ProviderType; 4] = [
        ProviderType::Box,
        ProviderType::Dropbox,
        ProviderType::Jottacloud,
        ProviderType::OpenDrive,
    ];

    /// A deliberately collision-heavy alphabet: source punctuation, control
    /// characters, the replacement runes those map to, the quote rune, the
    /// space/dot specials, and a couple of ordinary letters. Round-tripping all
    /// short strings over THIS set exercises every escape branch.
    const ALPHABET: &[char] = &[
        // source punctuation across the four tables
        ':', '?', '*', '"', '<', '>', '|', '\\', '%',
        // control + space + dot sources
        '\0', '\t', '\n', '\r', '\u{7F}', ' ', '.',
        // the replacement runes (the collision case)
        '：', '？', '＊', '＂', '＜', '＞', '｜', '＼', '％',
        '\u{2400}', '\u{2409}', '\u{2421}', '\u{2420}', '\u{FF0E}',
        // the quote rune + ordinary letters
        '\u{201B}', 'a', 'é',
    ];

    /// Every string of length 1..=`max` over `alphabet`, yielded to `f`. Each
    /// length-`len` string is the base-`n` expansion of a counter (a plain
    /// odometer over the alphabet indices).
    fn for_each_combo(alphabet: &[char], max: usize, mut f: impl FnMut(&str)) {
        let n = alphabet.len();
        let mut buf = String::new();
        for len in 1..=max {
            let total = n.pow(len as u32);
            for code in 0..total {
                buf.clear();
                let mut rest = code;
                for _ in 0..len {
                    buf.push(alphabet[rest % n]);
                    rest /= n;
                }
                f(&buf);
            }
        }
    }

    #[test]
    fn leaf_round_trips_for_all_providers() {
        for p in ENCODING_PROVIDERS {
            for_each_combo(ALPHABET, 3, |s| {
                let enc = encode_leaf(p, s);
                let dec = decode_leaf(p, &enc);
                assert_eq!(dec, s, "leaf round-trip failed for {p:?}: {s:?} -> {enc:?}");
            });
        }
    }

    #[test]
    fn path_round_trips_for_all_providers() {
        // Multi-segment paths with separators, leading/trailing/double slashes.
        let alphabet: Vec<char> = ALPHABET.iter().copied().chain(['/']).collect();
        for p in ENCODING_PROVIDERS {
            for_each_combo(&alphabet, 3, |s| {
                let enc = encode_path(p, s);
                let dec = decode_path(p, &enc);
                assert_eq!(dec, s, "path round-trip failed for {p:?}: {s:?} -> {enc:?}");
                // Separators and segment count are structurally preserved.
                assert_eq!(
                    enc.matches('/').count(),
                    s.matches('/').count(),
                    "separator count changed for {p:?}: {s:?} -> {enc:?}"
                );
            });
        }
    }

    #[test]
    fn punctuation_maps_to_fullwidth() {
        assert_eq!(encode_leaf(ProviderType::OpenDrive, "a:b"), "a：b");
        assert_eq!(encode_leaf(ProviderType::OpenDrive, "x?y*z"), "x？y＊z");
        assert_eq!(encode_leaf(ProviderType::Jottacloud, "100%"), "100％");
        assert_eq!(encode_leaf(ProviderType::Box, "a\\b"), "a＼b");
        // A provider that does not forbid the char leaves it alone.
        assert_eq!(encode_leaf(ProviderType::Box, "a:b"), "a:b");
        assert_eq!(encode_leaf(ProviderType::OpenDrive, "100%"), "100%");
    }

    #[test]
    fn control_chars_map_to_pictures() {
        assert_eq!(encode_leaf(ProviderType::Box, "a\tb"), "a\u{2409}b");
        assert_eq!(encode_leaf(ProviderType::Dropbox, "a\u{7F}b"), "a\u{2421}b");
        assert_eq!(encode_leaf(ProviderType::OpenDrive, "\0"), "\u{2400}");
    }

    #[test]
    fn whole_name_dot_is_encoded() {
        for p in ENCODING_PROVIDERS {
            assert_eq!(encode_leaf(p, "."), "\u{FF0E}");
            assert_eq!(encode_leaf(p, ".."), "\u{FF0E}\u{FF0E}");
            assert_eq!(decode_leaf(p, "\u{FF0E}"), ".");
            // A dot that is not the whole name is untouched (e.g. dotfiles).
            assert_eq!(encode_leaf(p, ".bashrc"), ".bashrc");
            assert_eq!(encode_leaf(p, "file."), "file.");
        }
    }

    #[test]
    fn position_dependent_space_rules() {
        // OpenDrive restricts both edges; Box/Dropbox only the trailing edge.
        assert_eq!(encode_leaf(ProviderType::OpenDrive, " a "), "\u{2420}a\u{2420}");
        assert_eq!(encode_leaf(ProviderType::Box, " a "), " a\u{2420}");
        assert_eq!(encode_leaf(ProviderType::Dropbox, "a "), "a\u{2420}");
        // An interior space is never touched.
        assert_eq!(encode_leaf(ProviderType::OpenDrive, "a b"), "a b");
    }

    #[test]
    fn literal_replacement_runes_are_escaped() {
        // A user whose name already contains a fullwidth colon must round-trip.
        let s = "a：b";
        let enc = encode_leaf(ProviderType::OpenDrive, s);
        assert_eq!(enc, "a\u{201B}：b");
        assert_eq!(decode_leaf(ProviderType::OpenDrive, &enc), s);
        // The quote rune itself escapes to a doubled quote rune.
        assert_eq!(encode_leaf(ProviderType::Box, "‛"), "‛‛");
        assert_eq!(decode_leaf(ProviderType::Box, "‛‛"), "‛");
    }

    #[test]
    fn non_encoding_providers_pass_through() {
        // S3 has no encode spec: names are returned verbatim, both directions.
        assert_eq!(encode_leaf(ProviderType::S3, "a:b?c"), "a:b?c");
        assert_eq!(decode_leaf(ProviderType::S3, "a：b"), "a：b");
        assert_eq!(encode_path(ProviderType::S3, "/a:b/c"), "/a:b/c");
    }

    #[test]
    fn path_preserves_slashes_and_emptiness() {
        assert_eq!(encode_path(ProviderType::OpenDrive, "/a:b/c?d/"), "/a：b/c？d/");
        assert_eq!(decode_path(ProviderType::OpenDrive, "/a：b/c？d/"), "/a:b/c?d/");
        assert_eq!(encode_path(ProviderType::OpenDrive, ""), "");
        assert_eq!(encode_path(ProviderType::OpenDrive, "/"), "/");
    }
}
