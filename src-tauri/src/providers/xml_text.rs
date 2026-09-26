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

/// Map an XML reference name (the text between `&` and `;`) to its
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
pub fn xml_entity_to_str(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        n if n.starts_with('#') => decode_numeric_ref(&n[1..]),
        _ => None,
    }
}

fn decode_numeric_ref(rest: &str) -> Option<String> {
    let codepoint = if let Some(hex) = rest.strip_prefix(['x', 'X']) {
        u32::from_str_radix(hex, 16).ok()?
    } else {
        rest.parse::<u32>().ok()?
    };
    char::from_u32(codepoint).map(|c| c.to_string())
}

/// Decode an XML attribute value, applying entity unescape and XML
/// attribute-value normalization.
///
/// quick-xml gives back attribute values as text (entities are NOT
/// auto-resolved on `attr.value`). For payloads where file names are
/// stored as attributes (e.g. Jottacloud `<file name="a&amp;b.txt">`),
/// the raw text still contains `&amp;` etc. and using it unchanged
/// produces the literal `a&amp;b.txt` instead of `a&b.txt`.
///
/// quick-xml 0.40 deprecated `unescape_value` in favour of
/// `normalized_value`, which additionally implements the XML 1.0
/// attribute-value normalization process (whitespace folding on
/// non-CDATA attributes). The normalized variant is the spec-correct
/// behaviour and resolves the same five builtin entities plus numeric
/// references.
///
/// Falls back to the raw attribute text if unescape fails (e.g. unknown
/// named reference): better to surface the raw value than to silently
/// drop the attribute.
pub fn attr_value(attr: &quick_xml::events::attributes::Attribute) -> String {
    // `Implicit1_0` matches the XML 1.0 assumption used by the spec when
    // the source has no `<?xml version="..."?>` prolog, which is the
    // overwhelmingly common case for the WebDAV / S3 / Azure / Jottacloud
    // payloads this helper feeds.
    attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| attr.value.to_string())
}

#[cfg(test)]
mod recorded_fragment_fixture {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    use super::{attr_value, xml_entity_to_str};

    #[test]
    fn parses_recorded_fragment() {
        let xml = include_str!("fixtures/quickxml/xml-text-fragment.xml");
        let mut reader = Reader::from_str(xml);
        reader.config_mut().trim_text(false);
        let mut buf = Vec::new();
        let mut attr_name = None;
        let mut text = String::new();
        loop {
            match reader.read_event_into(&mut buf) {
                Ok(Event::Start(e) | Event::Empty(e)) => {
                    if e.local_name().as_ref() == "file" {
                        if let Some(Ok(attr)) = e.attributes().next() {
                            attr_name = Some(attr_value(&attr));
                        }
                    }
                }
                Ok(Event::Text(e)) => {
                    let raw = e.as_ref();
                    if !raw.trim().is_empty() {
                        text.push_str(raw.trim());
                    }
                }
                Ok(Event::GeneralRef(e)) => {
                    if let Some(ch) = xml_entity_to_str(e.as_ref()) {
                        text.push_str(&ch);
                    }
                }
                Ok(Event::Eof) => break,
                Err(err) => panic!("fixture parse: {err}"),
                _ => {}
            }
            buf.clear();
        }
        assert_eq!(attr_name.as_deref(), Some("a&b.txt"));
        assert_eq!(text, "x&y");
    }
}
