//! Saved SITE command templates.
//!
//! A template is a SITE line with typed placeholders, stored on the server
//! profile. It holds no secret: a password is a `{name:secret}` hole that is
//! filled when the command is sent and is never written back. The GUI, the
//! CLI and the TUI share this module, so a line that one of them accepts is
//! the line the others send.
//!
//! The shareable glFTPd set ([`GLFTPD_CATALOG`]) is the command list from
//! discussion #1109. Importing it onto a profile copies the lines; it does
//! not decide that the connected server is glFTPd.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{json, Value};
use uuid::Uuid;
use zeroize::Zeroize;

use super::ftp_site::{SiteArgs, SiteInputError, MAX_SITE_LINE_BYTES};

/// How many templates one profile keeps.
pub const MAX_TEMPLATES: usize = 64;
/// Longest template name, in Unicode scalar values.
pub const MAX_NAME_CHARS: usize = 64;
/// Longest placeholder name, in bytes. The same bound as `--secret NAME`.
pub const MAX_SLOT_NAME_BYTES: usize = 32;
/// Most placeholders one line may have.
pub const MAX_SLOTS: usize = 8;
/// Profile JSON key. Absent means the profile has no templates.
pub const PROFILE_KEY: &str = "siteTemplates";
/// Mask used in the echo where a secret value was filled in.
pub const SECRET_MASK: &str = "••••••";

const GLFTPD_CATALOG_JSON: &str = include_str!("site_templates/glftpd.json");

/// Argument positions that are a password, per verb (0 is the verb). The same
/// positions the dialog already masks when the line is typed by hand.
const SECRET_POSITIONS: &[(&str, &[usize])] = &[
    ("ADDUSER", &[2]),
    ("GADDUSER", &[3]),
    ("CHPASS", &[2]),
    ("PASSWD", &[1]),
    ("PSWD", &[1, 2]),
];

/// What a placeholder asks for when the template is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// One argument, shown as typed.
    Text,
    /// Digits only.
    Number,
    /// Asked without echo, masked in the transcript, never stored.
    Secret,
}

impl SlotKind {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "text" => Some(Self::Text),
            "number" => Some(Self::Number),
            "secret" => Some(Self::Secret),
            _ => None,
        }
    }

    /// The name stored in a catalog and shown to the other surfaces.
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Number => "number",
            Self::Secret => "secret",
        }
    }
}

/// One `{name}` or `{name:kind}` in a template line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    pub kind: SlotKind,
    start: usize,
    end: usize,
}

impl Slot {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn kind(&self) -> SlotKind {
        self.kind
    }
}

/// A template as stored on a profile: an id, a name, and a line with holes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteTemplate {
    id: String,
    name: String,
    line: String,
    slots: Vec<Slot>,
}

impl SiteTemplate {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn line(&self) -> &str {
        &self.line
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    /// Check a line a person wants to save and give it a new id.
    pub fn create(name: &str, line: &str) -> Result<Self, TemplateError> {
        Self::from_parts(new_id(), name, line)
    }

    /// Rebuild a template the profile already stored. The id must be one
    /// [`Self::create`] issued.
    pub fn from_stored(id: &str, name: &str, line: &str) -> Result<Self, TemplateError> {
        Self::from_parts(id.to_string(), name, line)
    }

    fn from_parts(id: String, name: &str, line: &str) -> Result<Self, TemplateError> {
        if !is_template_id(&id) {
            return Err(TemplateError::BadId);
        }
        let name = checked_name(name)?;
        let line = normalize_line(line)?;
        let slots = parse_slots(&line)?;
        refuse_literal_password(&line, &slots)?;
        Ok(Self {
            id,
            name,
            line,
            slots,
        })
    }
}

/// The line after the holes are filled, plus the echo that may be shown.
pub struct Rendered {
    line: String,
    pub echo: String,
    pub carries_secret: bool,
}

impl Rendered {
    /// The arguments to send, the word `SITE` excluded.
    pub fn line(&self) -> &str {
        &self.line
    }
}

impl fmt::Debug for Rendered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Rendered")
            .field("echo", &self.echo)
            .field("carries_secret", &self.carries_secret)
            .finish_non_exhaustive()
    }
}

impl Drop for Rendered {
    fn drop(&mut self) {
        // The filled line can hold a password. The echo cannot.
        self.line.zeroize();
    }
}

/// Why a template was refused. The text never includes a slot value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateError {
    BadId,
    EmptyName,
    NameTooLong,
    EmptyLine,
    ControlCharacter,
    LineTooLong,
    /// A `{...}` that is not `{name}` or `{name:text|number|secret}`.
    BadPlaceholder {
        at: usize,
    },
    DuplicateSlot {
        name: String,
    },
    TooManySlots,
    /// A known password position holds a literal or a non-secret hole.
    PasswordMustBeSecret {
        verb: String,
    },
    TooManyTemplates,
    DuplicateName {
        name: String,
    },
    UnknownCatalog,
    UnknownTemplate {
        name: String,
    },
    AmbiguousTemplate {
        name: String,
    },
    MissingSlot {
        name: String,
    },
    UnexpectedSlot {
        name: String,
    },
    EmptyValue {
        name: String,
    },
    BadNumber {
        name: String,
    },
    ValueControl {
        name: String,
    },
    /// A space would split the SITE argument and shift whatever follows it.
    ValueWhitespace {
        name: String,
    },
    RenderedTooLong,
    /// The profile field is present and is not a template list.
    BadProfileField,
    /// A shared catalog has a field a template is not allowed to carry.
    UnexpectedField,
}

impl fmt::Display for TemplateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadId => write!(f, "the template id is not one this program issued"),
            Self::EmptyName => write!(f, "the template needs a name"),
            Self::NameTooLong => write!(
                f,
                "the template name is longer than {MAX_NAME_CHARS} characters"
            ),
            Self::EmptyLine => write!(f, "the template has no command"),
            Self::ControlCharacter => {
                write!(f, "the template contains a control character")
            }
            Self::LineTooLong => write!(
                f,
                "the template is longer than {MAX_SITE_LINE_BYTES} bytes"
            ),
            Self::BadPlaceholder { at } => write!(
                f,
                "the placeholder at byte {at} must be {{name}} or {{name:text}}, {{name:number}} or {{name:secret}}"
            ),
            Self::DuplicateSlot { name } => {
                write!(f, "the placeholder {{{name}}} is used more than once")
            }
            Self::TooManySlots => write!(f, "a template has at most {MAX_SLOTS} placeholders"),
            Self::PasswordMustBeSecret { verb } => write!(
                f,
                "{verb} carries a password: that argument must be a {{name:secret}} placeholder, not the password itself"
            ),
            Self::TooManyTemplates => {
                write!(f, "a profile keeps at most {MAX_TEMPLATES} templates")
            }
            Self::DuplicateName { name } => {
                write!(f, "a template named {name:?} is already on this profile")
            }
            Self::UnknownCatalog => write!(f, "unknown template catalog (the built-in one is glftpd)"),
            Self::UnknownTemplate { name } => write!(f, "no template named {name:?}"),
            Self::AmbiguousTemplate { name } => write!(
                f,
                "more than one template matches {name:?}: use the full name"
            ),
            Self::MissingSlot { name } => write!(f, "missing value for {{{name}}}"),
            Self::UnexpectedSlot { name } => {
                write!(f, "{{{name}}} is not a placeholder of this template")
            }
            Self::EmptyValue { name } => write!(f, "{{{name}}} is empty"),
            Self::BadNumber { name } => write!(f, "{{{name}}} must be digits"),
            Self::ValueControl { name } => {
                write!(f, "{{{name}}} contains a control character")
            }
            Self::ValueWhitespace { name } => {
                write!(f, "{{{name}}} contains whitespace")
            }
            Self::RenderedTooLong => write!(
                f,
                "the filled command is longer than {MAX_SITE_LINE_BYTES} bytes"
            ),
            Self::BadProfileField => write!(
                f,
                "siteTemplates on this profile is not a list of id, name and line"
            ),
            Self::UnexpectedField => write!(
                f,
                "a shared template file may only contain catalog, version, name and line"
            ),
        }
    }
}

/// `{NAME}` replaced by the matching `--secret` value, in one pass. Braces
/// that do not name a supplied secret stay as written: a raw SITE line is
/// not a template, and the server's own syntax may contain braces.
pub fn substitute_named_secrets(line: &str, secrets: &[(String, String)]) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let named = after.find('}').and_then(|close| {
            secrets
                .iter()
                .find(|(name, _)| *name == after[..close])
                .map(|(_, value)| (close, value.as_str()))
        });
        match named {
            Some((close, value)) => {
                out.push_str(value);
                rest = &after[close + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Fill every hole. A value is inserted once and not scanned again, so a
/// password that happens to contain `{other}` is sent as typed.
pub fn render(
    template: &SiteTemplate,
    values: &BTreeMap<String, String>,
) -> Result<Rendered, TemplateError> {
    for name in values.keys() {
        if !template.slots.iter().any(|slot| slot.name == *name) {
            return Err(TemplateError::UnexpectedSlot { name: name.clone() });
        }
    }
    let mut filled = String::with_capacity(template.line.len());
    let mut echo_body = String::with_capacity(template.line.len());
    let mut carries_secret = false;
    let mut cursor = 0usize;
    for slot in &template.slots {
        let value = values
            .get(&slot.name)
            .ok_or_else(|| TemplateError::MissingSlot {
                name: slot.name.clone(),
            })?;
        check_value(slot, value)?;
        filled.push_str(&template.line[cursor..slot.start]);
        echo_body.push_str(&template.line[cursor..slot.start]);
        filled.push_str(value);
        if slot.kind == SlotKind::Secret {
            carries_secret = true;
            echo_body.push_str(SECRET_MASK);
        } else {
            echo_body.push_str(value);
        }
        cursor = slot.end;
    }
    filled.push_str(&template.line[cursor..]);
    echo_body.push_str(&template.line[cursor..]);
    match SiteArgs::parse(&filled) {
        Ok(_) => {}
        Err(SiteInputError::TooLong) => return Err(TemplateError::RenderedTooLong),
        Err(SiteInputError::ControlCharacter) => {
            return Err(TemplateError::ControlCharacter);
        }
        Err(SiteInputError::Empty) => return Err(TemplateError::EmptyLine),
    }
    Ok(Rendered {
        line: filled,
        echo: format!("SITE {echo_body}"),
        carries_secret,
    })
}

/// The built-in glFTPd command set, as a shareable catalog with no secrets.
pub fn glftpd_catalog() -> &'static str {
    GLFTPD_CATALOG_JSON
}

pub fn builtin_catalog(name: &str) -> Result<&'static str, TemplateError> {
    if name.eq_ignore_ascii_case("glftpd") {
        Ok(GLFTPD_CATALOG_JSON)
    } else {
        Err(TemplateError::UnknownCatalog)
    }
}

/// Templates from a shared JSON catalog. Ids are new; the file has none.
pub fn parse_catalog(text: &str) -> Result<Vec<SiteTemplate>, TemplateError> {
    let value: Value = serde_json::from_str(text).map_err(|_| TemplateError::UnexpectedField)?;
    let obj = value.as_object().ok_or(TemplateError::UnexpectedField)?;
    for key in obj.keys() {
        if key != "catalog" && key != "version" && key != "templates" {
            return Err(TemplateError::UnexpectedField);
        }
    }
    let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
    if version != 1 {
        return Err(TemplateError::UnexpectedField);
    }
    let entries = obj
        .get("templates")
        .and_then(Value::as_array)
        .ok_or(TemplateError::UnexpectedField)?;
    if entries.len() > MAX_TEMPLATES {
        return Err(TemplateError::TooManyTemplates);
    }
    let mut out = Vec::with_capacity(entries.len());
    let mut names: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.as_object().ok_or(TemplateError::UnexpectedField)?;
        for key in entry.keys() {
            if key != "name" && key != "line" {
                return Err(TemplateError::UnexpectedField);
            }
        }
        let name = entry
            .get("name")
            .and_then(Value::as_str)
            .ok_or(TemplateError::UnexpectedField)?;
        let line = entry
            .get("line")
            .and_then(Value::as_str)
            .ok_or(TemplateError::UnexpectedField)?;
        if names
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(name))
        {
            return Err(TemplateError::DuplicateName {
                name: name.to_string(),
            });
        }
        names.push(name.to_string());
        out.push(SiteTemplate::create(name, line)?);
    }
    Ok(out)
}

/// A catalog document for the templates on a profile. No ids, no values.
pub fn export_catalog(templates: &[SiteTemplate]) -> Value {
    json!({
        "catalog": "profile",
        "version": 1,
        "templates": templates.iter().map(|template| json!({
            "name": template.name,
            "line": template.line,
        })).collect::<Vec<_>>(),
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub added: Vec<String>,
    pub skipped: Vec<String>,
}

/// Add catalog templates whose name is not already on the profile. A name
/// that is already there, ignoring ASCII case, is left as the profile has it.
pub fn merge_import(
    existing: &[SiteTemplate],
    incoming: &[SiteTemplate],
) -> Result<(Vec<SiteTemplate>, ImportReport), TemplateError> {
    let mut added = Vec::new();
    let mut skipped = Vec::new();
    let mut next = existing.to_vec();
    for template in incoming {
        if next
            .iter()
            .any(|have| have.name.eq_ignore_ascii_case(&template.name))
        {
            skipped.push(template.name.clone());
            continue;
        }
        if next.len() >= MAX_TEMPLATES {
            return Err(TemplateError::TooManyTemplates);
        }
        next.push(template.clone());
        added.push(template.name.clone());
    }
    Ok((next, ImportReport { added, skipped }))
}

/// Read `siteTemplates`. A missing field is an empty list. A field of the
/// wrong shape is an error, so a damaged list is not silently dropped.
pub fn read_profile_templates(profile: &Value) -> Result<Vec<SiteTemplate>, TemplateError> {
    let Some(field) = profile.get(PROFILE_KEY) else {
        return Ok(Vec::new());
    };
    let entries = field.as_array().ok_or(TemplateError::BadProfileField)?;
    if entries.len() > MAX_TEMPLATES {
        return Err(TemplateError::TooManyTemplates);
    }
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let obj = entry.as_object().ok_or(TemplateError::BadProfileField)?;
        if obj.len() != 3
            || !obj.contains_key("id")
            || !obj.contains_key("name")
            || !obj.contains_key("line")
        {
            return Err(TemplateError::BadProfileField);
        }
        let id = obj["id"].as_str().ok_or(TemplateError::BadProfileField)?;
        let name = obj["name"].as_str().ok_or(TemplateError::BadProfileField)?;
        let line = obj["line"].as_str().ok_or(TemplateError::BadProfileField)?;
        let template = SiteTemplate::from_parts(id.to_string(), name, line)?;
        if out.iter().any(|have: &SiteTemplate| {
            have.name.eq_ignore_ascii_case(&template.name) || have.id == template.id
        }) {
            return Err(TemplateError::DuplicateName {
                name: template.name.clone(),
            });
        }
        out.push(template);
    }
    Ok(out)
}

/// Write the list back. An empty list removes the field.
pub fn write_profile_templates(profile: &mut Value, templates: &[SiteTemplate]) {
    let Some(obj) = profile.as_object_mut() else {
        return;
    };
    if templates.is_empty() {
        obj.remove(PROFILE_KEY);
        return;
    }
    obj.insert(
        PROFILE_KEY.to_string(),
        json!(templates
            .iter()
            .map(|template| json!({
                "id": template.id,
                "name": template.name,
                "line": template.line,
            }))
            .collect::<Vec<_>>()),
    );
}

/// Resolve a template the way `--profile` resolves a server: exact name
/// (case-insensitive), then a unique substring.
pub fn find_template<'a>(
    templates: &'a [SiteTemplate],
    query: &str,
) -> Result<&'a SiteTemplate, TemplateError> {
    let query_lower = query.to_lowercase();
    if let Some(template) = templates
        .iter()
        .find(|template| template.name.to_lowercase() == query_lower)
    {
        return Ok(template);
    }
    let matches: Vec<_> = templates
        .iter()
        .filter(|template| template.name.to_lowercase().contains(&query_lower))
        .collect();
    match matches.len() {
        1 => Ok(matches[0]),
        0 => Err(TemplateError::UnknownTemplate {
            name: query.to_string(),
        }),
        _ => Err(TemplateError::AmbiguousTemplate {
            name: query.to_string(),
        }),
    }
}

fn new_id() -> String {
    format!("tpl_{}", Uuid::new_v4().simple())
}

fn is_template_id(id: &str) -> bool {
    let Some(hex) = id.strip_prefix("tpl_") else {
        return false;
    };
    hex.len() == 32
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn checked_name(name: &str) -> Result<String, TemplateError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(TemplateError::EmptyName);
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(TemplateError::NameTooLong);
    }
    if name.chars().any(char::is_control) {
        return Err(TemplateError::ControlCharacter);
    }
    Ok(name.to_string())
}

fn normalize_line(raw: &str) -> Result<String, TemplateError> {
    match SiteArgs::parse(raw) {
        Ok(args) => Ok(args.arguments().to_string()),
        Err(SiteInputError::Empty) => Err(TemplateError::EmptyLine),
        Err(SiteInputError::ControlCharacter) => Err(TemplateError::ControlCharacter),
        Err(SiteInputError::TooLong) => Err(TemplateError::LineTooLong),
    }
}

fn parse_slots(line: &str) -> Result<Vec<Slot>, TemplateError> {
    let mut slots = Vec::new();
    let mut index = 0usize;
    while index < line.len() {
        let ch = line[index..].chars().next().expect("index is on a char");
        if ch == '{' {
            let start = index;
            let Some(rel) = line[index + 1..].find('}') else {
                return Err(TemplateError::BadPlaceholder { at: start });
            };
            let end = index + 1 + rel + 1;
            let inner = &line[index + 1..end - 1];
            let (name, kind) =
                parse_inner(inner).ok_or(TemplateError::BadPlaceholder { at: start })?;
            if slots.iter().any(|slot: &Slot| slot.name == name) {
                return Err(TemplateError::DuplicateSlot { name });
            }
            if slots.len() == MAX_SLOTS {
                return Err(TemplateError::TooManySlots);
            }
            slots.push(Slot {
                name,
                kind,
                start,
                end,
            });
            index = end;
        } else if ch == '}' {
            return Err(TemplateError::BadPlaceholder { at: index });
        } else {
            index += ch.len_utf8();
        }
    }
    Ok(slots)
}

fn parse_inner(inner: &str) -> Option<(String, SlotKind)> {
    if inner.is_empty() || inner.len() > MAX_SLOT_NAME_BYTES + ":secret".len() {
        return None;
    }
    let (name, kind) = match inner.split_once(':') {
        Some((name, kind)) => (name, SlotKind::parse(kind)?),
        None => (inner, SlotKind::Text),
    };
    if name.len() > MAX_SLOT_NAME_BYTES || !is_slot_name(name) {
        return None;
    }
    Some((name.to_string(), kind))
}

fn is_slot_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn refuse_literal_password(line: &str, slots: &[Slot]) -> Result<(), TemplateError> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let (verb, args) = match words.first() {
        Some(verb) => (verb.to_ascii_uppercase(), &words[1..]),
        None => return Ok(()),
    };
    let Some(positions) = SECRET_POSITIONS
        .iter()
        .find(|(name, _)| *name == verb)
        .map(|(_, positions)| *positions)
    else {
        return Ok(());
    };
    // Positions in SECRET_POSITIONS count the verb as 0. `args` starts at 1.
    for position in positions {
        let arg_index = position.saturating_sub(1);
        let Some(word) = args.get(arg_index) else {
            continue;
        };
        let secret_hole = slots
            .iter()
            .any(|slot| slot.kind == SlotKind::Secret && line[slot.start..slot.end] == **word);
        if !secret_hole {
            return Err(TemplateError::PasswordMustBeSecret { verb });
        }
    }
    Ok(())
}

fn check_value(slot: &Slot, value: &str) -> Result<(), TemplateError> {
    if value.is_empty() {
        return Err(TemplateError::EmptyValue {
            name: slot.name.clone(),
        });
    }
    // Whitespace is not only a control character: a space would split the
    // argument and move a later secret into the wrong position.
    if value.chars().any(char::is_whitespace) {
        return Err(TemplateError::ValueWhitespace {
            name: slot.name.clone(),
        });
    }
    if value.chars().any(char::is_control) {
        return Err(TemplateError::ValueControl {
            name: slot.name.clone(),
        });
    }
    if slot.kind == SlotKind::Number
        && (value.len() > 18 || !value.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(TemplateError::BadNumber {
            name: slot.name.clone(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    #[test]
    fn a_literal_password_is_refused_and_a_secret_hole_is_kept() {
        let literal = SiteTemplate::create("bad", "CHPASS alice hunter2");
        assert!(matches!(
            literal,
            Err(TemplateError::PasswordMustBeSecret { .. })
        ));
        let text_hole = SiteTemplate::create("bad", "CHPASS {user} {pw}");
        assert!(matches!(
            text_hole,
            Err(TemplateError::PasswordMustBeSecret { .. })
        ));
        let template = SiteTemplate::create("Change password", "SITE CHPASS {user} {pw:secret}")
            .expect("secret hole");
        assert_eq!(template.line(), "CHPASS {user} {pw:secret}");
        let rendered =
            render(&template, &values(&[("user", "alice"), ("pw", "hunter2")])).expect("render");
        assert_eq!(rendered.line(), "CHPASS alice hunter2");
        assert_eq!(rendered.echo, "SITE CHPASS alice ••••••");
        assert!(rendered.carries_secret);
        assert!(
            !format!("{rendered:?}").contains("hunter2"),
            "debug printed the password: {rendered:?}"
        );
    }

    #[test]
    fn a_slot_value_with_whitespace_cannot_shift_a_later_argument() {
        let template =
            SiteTemplate::create("Change password", "CHPASS {user} {pw:secret}").unwrap();
        assert!(matches!(
            render(
                &template,
                &values(&[("user", "alice hunter2"), ("pw", "secret")])
            ),
            Err(TemplateError::ValueWhitespace { .. })
        ));
        assert!(matches!(
            render(&template, &values(&[("user", "alice"), ("pw", "one two")])),
            Err(TemplateError::ValueWhitespace { .. })
        ));
    }

    #[test]
    fn a_value_is_inserted_once_and_a_number_is_digits() {
        let template = SiteTemplate::create("User", "USER {user}").unwrap();
        let rendered = render(&template, &values(&[("user", "{pw}")])).unwrap();
        assert_eq!(rendered.line(), "USER {pw}");
        let ratio = SiteTemplate::create("Ratio", "CHANGE {user} ratio {ratio:number}").unwrap();
        assert!(matches!(
            render(&ratio, &values(&[("user", "alice"), ("ratio", "5a")])),
            Err(TemplateError::BadNumber { .. })
        ));
        assert_eq!(
            render(&ratio, &values(&[("user", "alice"), ("ratio", "022")]))
                .unwrap()
                .line(),
            "CHANGE alice ratio 022"
        );
    }

    #[test]
    fn braces_that_are_not_a_placeholder_are_refused() {
        assert!(matches!(
            SiteTemplate::create("bad", "WHO {"),
            Err(TemplateError::BadPlaceholder { .. })
        ));
        assert!(matches!(
            SiteTemplate::create("bad", "WHO }"),
            Err(TemplateError::BadPlaceholder { .. })
        ));
        assert!(matches!(
            SiteTemplate::create("bad", "USER {user} {user}"),
            Err(TemplateError::DuplicateSlot { .. })
        ));
        assert!(matches!(
            SiteTemplate::create("bad", "USER {user:Secret}"),
            Err(TemplateError::BadPlaceholder { .. })
        ));
    }

    #[test]
    fn named_secret_substitution_is_one_pass_and_leaves_other_braces() {
        let line = substitute_named_secrets(
            "CHPASS {user} {pw}",
            &[("pw".to_string(), "a{user}b".to_string())],
        );
        assert_eq!(line, "CHPASS {user} a{user}b");
        assert_eq!(
            substitute_named_secrets("WHO {not-a-secret}", &[]),
            "WHO {not-a-secret}"
        );
    }

    #[test]
    fn the_glftpd_catalog_imports_without_secrets_and_does_not_overwrite() {
        let incoming = parse_catalog(glftpd_catalog()).expect("catalog");
        assert!(incoming.iter().any(|template| template.name() == "Who"));
        assert!(incoming.iter().any(|template| {
            template.name() == "Add user" && template.line().contains("{pw:secret}")
        }));
        let exported = export_catalog(&incoming).to_string();
        assert!(!exported.contains("hunter2"));
        assert!(!exported.contains("\"id\""));
        let custom = SiteTemplate::create("Who", "WHO -hidden").unwrap();
        let (merged, report) = merge_import(std::slice::from_ref(&custom), &incoming).unwrap();
        assert_eq!(report.skipped, vec!["Who".to_string()]);
        assert_eq!(merged[0].line(), "WHO -hidden");
        assert_eq!(merged.len(), incoming.len());
        let folded = SiteTemplate::create("who", "WHO -hidden").unwrap();
        let (folded_merged, folded_report) = merge_import(&[folded], &incoming).unwrap();
        assert_eq!(folded_report.skipped, vec!["Who".to_string()]);
        assert_eq!(folded_merged[0].line(), "WHO -hidden");
        assert!(parse_catalog(
            r#"{"catalog":"x","version":1,"templates":[{"name":"Who","line":"WHO"},{"name":"who","line":"STAT"}]}"#
        )
        .is_err());
    }

    #[test]
    fn a_profile_round_trip_drops_nothing_and_rejects_an_extra_field() {
        let template = SiteTemplate::create("User", "USER {user}").unwrap();
        let mut profile = json!({"id": "srv_1", "name": "Lab", "protocol": "ftps"});
        write_profile_templates(&mut profile, std::slice::from_ref(&template));
        let read = read_profile_templates(&profile).unwrap();
        assert_eq!(read[0].line(), "USER {user}");
        assert_eq!(profile["name"], "Lab");
        profile["siteTemplates"][0]["password"] = json!("hunter2");
        assert_eq!(
            read_profile_templates(&profile),
            Err(TemplateError::BadProfileField)
        );
        write_profile_templates(&mut profile, &[]);
        assert!(profile.get("siteTemplates").is_none());
        assert_eq!(profile["name"], "Lab");
    }

    #[test]
    fn a_full_profile_refuses_one_more_template() {
        let existing: Vec<_> = (0..MAX_TEMPLATES)
            .map(|index| SiteTemplate::create(&format!("T{index}"), "WHO").unwrap())
            .collect();
        let extra = SiteTemplate::create("One more", "STAT").unwrap();
        assert_eq!(
            merge_import(&existing, &[extra]),
            Err(TemplateError::TooManyTemplates)
        );
    }
}
