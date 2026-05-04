//! Small helpers for building text from quick-xml event streams.
//!
//! quick-xml does not auto-resolve entity references inside `<Element>`
//! text content. For an XML fragment like `<Key>a&amp;b</Key>` it emits:
//!
//!   Event::Text("a")  +  Event::GeneralRef("amp")  +  Event::Text("b")
//!
//! Parsers that simply do `field = String::from_utf8_lossy(text)` on
//! each `Event::Text` overwrite the first fragment ("a") with the last
//! ("b") and silently drop the entity. The result is a key listed as
//! `b` instead of `a&b`: and downstream operations (delete/get) then
//! act on the wrong key. This is exactly what triggered the
//! `aeroftp-encoding-test/a&b.txt` regression on Storj.
//!
//! Use this module to:
//!   1. accumulate `Event::Text` fragments via `push_str` rather than
//!      assigning,
//!   2. translate the five XML-builtin entities (`amp`, `lt`, `gt`,
//!      `quot`, `apos`) emitted as `Event::GeneralRef` into their
//!      single-character expansion.
//!
//! Numeric character references (`&#39;`, `&#x27;`) are also handled -
//! Storj's S3 gateway in particular emits `&#39;` instead of `&apos;`
//! for U+0027 in object keys, so a builtin-only translator silently
//! drops apostrophes in listed file names.

/// Map an XML reference name (the bytes between `&` and `;`) to its
/// expansion as an owned `String`.
///
/// Handles:
///   - the five XML-builtin named entities (`amp`, `lt`, `gt`, `quot`,
///     `apos`);
///   - decimal numeric character references (`#39`, `#10`, ...);
///   - hex numeric character references (`#x27`, `#xA0`, ...).
///
/// Returns `None` for unsupported / unknown names and for numeric refs
/// that don't decode to a valid Unicode scalar value; callers should
/// treat those as "skip / leave the surrounding text unchanged".
pub fn xml_entity_to_str(name: &[u8]) -> Option<String> {
    match name {
        b"amp" => Some("&".to_string()),
        b"lt" => Some("<".to_string()),
        b"gt" => Some(">".to_string()),
        b"quot" => Some("\"".to_string()),
        b"apos" => Some("'".to_string()),
        n if n.first() == Some(&b'#') => decode_numeric_ref(&n[1..]),
        _ => None,
    }
}

fn decode_numeric_ref(rest: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(rest).ok()?;
    let codepoint = if let Some(hex) = s.strip_prefix(['x', 'X']) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        s.parse::<u32>().ok()?
    };
    char::from_u32(codepoint).map(|c| c.to_string())
}

/// Decode an XML attribute value, applying entity unescape.
///
/// quick-xml gives back attribute values verbatim (entities are NOT
/// auto-resolved on `attr.value`). For payloads where file names are
/// stored as attributes (e.g. Jottacloud `<file name="a&amp;b.txt">`),
/// the raw bytes still contain `&amp;` etc. and a plain UTF-8 decode
/// produces the literal `a&amp;b.txt` instead of `a&b.txt`. Calling
/// `Attribute::unescape_value()` resolves the five XML-builtin entities
/// and decimal/hex numeric references.
///
/// Falls back to a lossy UTF-8 decode if unescape fails (e.g. unknown
/// named reference): better to surface the raw value than to silently
/// drop the attribute.
pub fn attr_value(attr: &quick_xml::events::attributes::Attribute) -> String {
    attr.unescape_value()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| String::from_utf8_lossy(&attr.value).to_string())
}
