//! `SITE` commands sent through an open FTP/FTPS session.
//!
//! `SITE` is how servers expose what RFC 959 leaves to them: glFTPd runs its
//! whole account administration through it (`SITE USER`, `SITE CHANGE`,
//! `SITE ADDUSER`...). The GUI, the TUI and the CLI all go through
//! [`run_site_command`], so the three show the same outcomes for the same
//! exchange.
//!
//! Three rules shape everything here:
//!
//! - **A command that may have reached the server is never sent again.** An
//!   account command is not idempotent, and after a timeout or a dropped
//!   connection nobody knows whether the server applied it. The session is
//!   rebuilt so the user can go on working, and the outcome is reported as
//!   unknown.
//! - **The arguments are never logged.** `SITE ADDUSER` and `SITE CHPASS`
//!   carry passwords, and a server-defined command can carry anything, so
//!   only the outcome, the reply code and the timing are logged. The reply
//!   text is not logged either: it names users, IP masks and idents.
//! - **The reply code does not say whether the command worked.** glFTPd
//!   answers `200 Invalid Access...` to a refused change. The code and every
//!   line are reported as received; nothing here calls a reply a success.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use std::time::Duration;

use serde::Serialize;

use super::{FtpProvider, StorageProvider};

/// Longest `SITE` line accepted, `SITE ` included, CRLF excluded. RFC 959
/// sets no limit, many servers cut at 512, and glFTPd account commands are
/// far shorter; 1024 leaves room for long custom commands without letting a
/// paste of a whole file through.
pub const MAX_SITE_LINE_BYTES: usize = 1024;

/// Most bytes one `SITE` exchange may bring back, preliminary replies
/// included: suppaftp's cap is per reply, and a server that answers with
/// endless `1xx` lines would otherwise grow the exchange without limit. The
/// same 256 KiB suppaftp allows a single reply.
pub const MAX_SITE_REPLY_BYTES: usize = 256 * 1024;

/// Default wait for the complete reply. Account commands answer in
/// milliseconds, but servers also run slow scripts through `SITE` (a glFTPd
/// `SITE RESCAN` or `SITE WIPE -r` can take minutes), and a deadline that is
/// too short turns a working command into an unknown outcome.
pub const DEFAULT_REPLY_TIMEOUT: Duration = Duration::from_secs(60);
/// How long an exchange waits after its final reply for a surplus one. A
/// server that sends a second final reply late would otherwise have it read
/// as the next command's answer (a CLI batch sends its next line at once).
/// Bounded and short: it catches a late surplus, it does not prove that
/// nothing can arrive afterwards, and the next command's pre-flight check
/// still catches what lands later.
pub const DEFAULT_SETTLE: Duration = Duration::from_millis(50);
/// Shortest wait a caller may ask for.
pub const MIN_REPLY_TIMEOUT: Duration = Duration::from_secs(1);
/// Longest wait a caller may ask for.
pub const MAX_REPLY_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Verbs whose reply only reads server state, on the servers that define
/// them (glFTPd, ProFTPD `QUOTA`, Pure-FTPd `TIME`, Serv-U `ZONE`). Used only
/// to skip a listing refresh: a verb missing from here is treated as one that
/// may change files, which costs one refresh at worst.
const READ_ONLY_VERBS: &[&str] = &[
    "ALDN", "ALUP", "DAYDN", "DAYUP", "DUPE", "ERRLOG", "FLAGS", "GINFO", "GROUPS", "GRP", "HELP",
    "LASTON", "LOGINS", "MONTHDN", "MONTHUP", "NEW", "NUKES", "QUOTA", "SEARCH", "SEEN", "STAT",
    "STATS", "SWHO", "SYSLOG", "TIME", "TRAFFIC", "USER", "USERS", "VERS", "WHO", "WKDN", "WKUP",
    "ZONE",
];

/// Verbs that change how the server treats this connection: glFTPd `COLOR`
/// puts colour codes into replies and listings, `IDLE` the idle timeout
/// (glFTPd, Pure-FTPd), vsftpd `UMASK`, IIS `DIRSTYLE` the listing format,
/// FileZilla Server 0.9 `NAMEFMT`. They reach only the browsing connection:
/// transfers run on connections of their own.
const SESSION_STATE_VERBS: &[&str] = &["COLOR", "DIRSTYLE", "IDLE", "NAMEFMT", "UMASK"];

/// What is known about a verb, for the surfaces' warnings and refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerbKind {
    /// Known to read server state only.
    ReadOnly,
    /// Known to change how the server treats this connection.
    SessionState,
    /// Anything else, unknown verbs included: may change files or accounts.
    Other,
}

/// Why a line was refused before anything was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SiteInputError {
    /// Nothing to send once the optional `SITE` prefix is removed.
    Empty,
    /// A control character (CR, LF, NUL, TAB, DEL...) inside the line: CR or
    /// LF would smuggle a second command into the session.
    ControlCharacter,
    /// Longer than [`MAX_SITE_LINE_BYTES`].
    TooLong,
}

/// A validated `SITE` line. The arguments leave this type only toward the
/// wire; [`SiteArgs::label`] is the part that may be shown in logs and
/// machine output.
#[derive(Clone, PartialEq, Eq)]
pub struct SiteArgs {
    line: String,
    verb: Option<String>,
}

impl std::fmt::Debug for SiteArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteArgs")
            .field("verb", &self.verb)
            .finish_non_exhaustive()
    }
}

impl SiteArgs {
    /// Validate what a person typed. Surrounding whitespace, a line break
    /// left by a paste included, is trimmed, and one leading `SITE` word is
    /// dropped, so `SITE WHO` and `WHO` send the same line. A control
    /// character inside the line is refused, never removed: a removed CR/LF
    /// would turn two commands into one silently.
    pub fn parse(raw: &str) -> Result<Self, SiteInputError> {
        Self::checked(strip_site_prefix(raw.trim()).to_string())
    }

    /// Validate a line built by the program (for example `CHMOD 644 <path>`),
    /// sent exactly as given: a path may begin or end with a space, contain a
    /// TAB, or be long. Only what would end the line early or smuggle a second
    /// command is refused (CR, LF, NUL), as suppaftp did before this path
    /// existed; the stricter rules of [`SiteArgs::parse`] are for typed input.
    pub(crate) fn verbatim(line: String) -> Result<Self, SiteInputError> {
        if line.trim().is_empty() {
            return Err(SiteInputError::Empty);
        }
        if line.contains(['\r', '\n', '\0']) {
            return Err(SiteInputError::ControlCharacter);
        }
        Ok(Self::with_verb(line))
    }

    fn checked(line: String) -> Result<Self, SiteInputError> {
        if line.trim().is_empty() {
            return Err(SiteInputError::Empty);
        }
        if line.chars().any(char::is_control) {
            return Err(SiteInputError::ControlCharacter);
        }
        if "SITE ".len() + line.len() > MAX_SITE_LINE_BYTES {
            return Err(SiteInputError::TooLong);
        }
        Ok(Self::with_verb(line))
    }

    fn with_verb(line: String) -> Self {
        let verb = line
            .split_whitespace()
            .next()
            .filter(|word| is_plausible_verb(word))
            .map(str::to_ascii_uppercase);
        Self { line, verb }
    }

    /// The control-channel line, CRLF excluded.
    pub(crate) fn wire_line(&self) -> String {
        format!("SITE {}", self.line)
    }

    /// `SITE <VERB>`, or `SITE` when the first word does not look like a
    /// verb. This keeps a password with symbols, or longer than 16
    /// characters, out of labels when it is pasted alone; a short
    /// alphanumeric one (`hunter2`) still reads as a verb, since nothing tells
    /// it apart from a custom command such as `REQFILLED`. A declared limit.
    pub fn label(&self) -> String {
        site_label(self.verb.as_deref())
    }

    /// What is known about the verb.
    pub fn verb_kind(&self) -> VerbKind {
        match self.verb.as_deref() {
            Some(verb) if READ_ONLY_VERBS.contains(&verb) => VerbKind::ReadOnly,
            Some(verb) if SESSION_STATE_VERBS.contains(&verb) => VerbKind::SessionState,
            _ => VerbKind::Other,
        }
    }
}

fn site_label(verb: Option<&str>) -> String {
    match verb {
        Some(verb) => format!("SITE {verb}"),
        None => "SITE".to_string(),
    }
}

/// A verb is a short ASCII word; anything else is kept out of labels.
fn is_plausible_verb(word: &str) -> bool {
    (1..=16).contains(&word.len()) && word.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn strip_site_prefix(line: &str) -> &str {
    let mut parts = line.splitn(2, char::is_whitespace);
    match (parts.next(), parts.next()) {
        (Some(first), rest) if first.eq_ignore_ascii_case("SITE") => {
            rest.unwrap_or("").trim_start()
        }
        _ => line,
    }
}

/// How the reply bytes were turned into text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyEncoding {
    Utf8,
    /// Not valid UTF-8, decoded byte for byte as Latin-1 so nothing is lost.
    /// Legacy servers send Latin-1 or CP437 box drawing.
    Latin1,
}

/// A complete reply: the final code and every line as the server sent it,
/// code prefixes included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteReply {
    pub code: u16,
    pub lines: Vec<String>,
    pub encoding: ReplyEncoding,
}

impl SiteReply {
    /// `None` when the last line carries no three-digit code, which suppaftp
    /// never returns for a complete reply.
    pub(crate) fn from_body(body: &[u8]) -> Option<Self> {
        let (text, encoding) = match std::str::from_utf8(body) {
            Ok(text) => (text.to_string(), ReplyEncoding::Utf8),
            Err(_) => (
                body.iter().map(|&b| char::from(b)).collect(),
                ReplyEncoding::Latin1,
            ),
        };
        let mut lines: Vec<String> = text
            .split('\n')
            .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
            .collect();
        if lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }
        let code = final_reply_code(&lines)?;
        Some(Self {
            code,
            lines,
            encoding,
        })
    }
}

/// The code of the last line that has one. suppaftp maps codes it does not
/// list (299, 534...) to `Status::Unknown`, whose number is 0, so the code is
/// read from the text instead.
pub(crate) fn final_reply_code(lines: &[String]) -> Option<u16> {
    lines
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .and_then(|line| line.get(..3))
        .filter(|digits| digits.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|digits| digits.parse().ok())
}

/// The final code of raw reply bytes, for the preliminary-reply check.
pub(crate) fn final_reply_code_of(body: &[u8]) -> Option<u16> {
    SiteReply::from_body(body).map(|reply| reply.code)
}

/// A reply line without its `NNN-` / `NNN ` prefix, for reading. Lines of a
/// multi-line reply that carry no prefix are returned unchanged.
pub fn without_reply_code(line: &str) -> &str {
    let bytes = line.as_bytes();
    let has_code = bytes.len() >= 3 && bytes[..3].iter().all(u8::is_ascii_digit);
    match (has_code, bytes.get(3)) {
        (true, None) => "",
        (true, Some(b'-' | b' ')) => &line[4..],
        _ => line,
    }
}

/// `line` without ANSI escape sequences (glFTPd `SITE COLOR` puts colour
/// codes in replies), for surfaces that cannot render them.
pub fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI: ESC [ parameters, intermediates, one final byte in @..~
            Some('[') => {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // Any other escape: ESC plus one character.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Options for one exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiteOptions {
    /// Wait for the complete reply, preliminary replies included.
    pub reply_timeout: Duration,
    /// Wait after the final reply for a surplus one ([`DEFAULT_SETTLE`]);
    /// zero checks once without waiting.
    pub settle: Duration,
}

impl Default for SiteOptions {
    fn default() -> Self {
        Self {
            reply_timeout: DEFAULT_REPLY_TIMEOUT,
            settle: DEFAULT_SETTLE,
        }
    }
}

impl SiteOptions {
    /// Options with the reply wait in seconds, clamped to
    /// [`MIN_REPLY_TIMEOUT`]..=[`MAX_REPLY_TIMEOUT`].
    pub fn with_reply_timeout_secs(secs: u64) -> Self {
        Self {
            reply_timeout: Duration::from_secs(secs).clamp(MIN_REPLY_TIMEOUT, MAX_REPLY_TIMEOUT),
            ..Self::default()
        }
    }

    /// The same options with another settle window.
    pub fn with_settle(self, settle: Duration) -> Self {
        Self { settle, ..self }
    }
}

/// Why nothing was sent. In every case the server received no byte of the
/// command, so sending it again by hand is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotSentReason {
    Input(SiteInputError),
    /// The session is not FTP or FTPS.
    NotFtp,
    /// No session to send on, and none to redial.
    NotConnected,
    /// The session is busy with other work (GUI: a transfer holds it).
    Busy,
    /// The session needed a redial before sending, and the redial failed.
    ReconnectFailed,
}

/// Why no complete reply was read after the command was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownCause {
    Timeout,
    ConnectionLost,
    /// The reply did not follow RFC 959.
    MalformedReply,
    /// The reply passed suppaftp's 256 KiB limit and was abandoned half read.
    ReplyTooLarge,
}

/// The outcome of one exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SiteOutcome {
    /// The server sent a complete final reply, whatever its code.
    Replied {
        reply: SiteReply,
        elapsed: Duration,
        /// The server sent more than this reply, so the session was redialed
        /// to keep the next command from reading the surplus as its answer.
        session_reset: bool,
    },
    /// Nothing was sent.
    NotSent(NotSentReason),
    /// The command may have reached the server, and no complete reply came.
    /// It was not sent again.
    Unknown {
        cause: UnknownCause,
        elapsed: Duration,
        /// Whether the session was rebuilt afterwards.
        session_reconnected: bool,
    },
}

impl SiteOutcome {
    /// The fixed word logged for this outcome; never the arguments.
    pub fn kind(&self) -> &'static str {
        match self {
            SiteOutcome::Replied { .. } => "replied",
            SiteOutcome::NotSent(_) => "not_sent",
            SiteOutcome::Unknown { .. } => "unknown",
        }
    }

    fn reason(&self) -> Option<&'static str> {
        match self {
            SiteOutcome::Replied { .. } => None,
            SiteOutcome::NotSent(reason) => Some(match reason {
                NotSentReason::Input(SiteInputError::Empty) => "empty",
                NotSentReason::Input(SiteInputError::ControlCharacter) => "control_character",
                NotSentReason::Input(SiteInputError::TooLong) => "too_long",
                NotSentReason::NotFtp => "not_ftp",
                NotSentReason::NotConnected => "not_connected",
                NotSentReason::Busy => "busy",
                NotSentReason::ReconnectFailed => "reconnect_failed",
            }),
            SiteOutcome::Unknown { cause, .. } => Some(match cause {
                UnknownCause::Timeout => "timeout",
                UnknownCause::ConnectionLost => "connection_lost",
                UnknownCause::MalformedReply => "malformed_reply",
                UnknownCause::ReplyTooLarge => "reply_too_large",
            }),
        }
    }
}

/// One exchange with what the surfaces may show about the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteRun {
    /// `SITE <VERB>` or `SITE`, never the arguments.
    pub label: String,
    pub verb_kind: VerbKind,
    pub outcome: SiteOutcome,
}

impl SiteRun {
    /// A run that never reached a session, for a refusal decided by the
    /// caller (the GUI's busy session).
    pub fn not_sent(raw: &str, reason: NotSentReason) -> Self {
        let (label, verb_kind) = match SiteArgs::parse(raw) {
            Ok(args) => (args.label(), args.verb_kind()),
            Err(_) => (site_label(None), VerbKind::Other),
        };
        Self {
            label,
            verb_kind,
            outcome: SiteOutcome::NotSent(reason),
        }
    }

    /// The document the GUI receives and `aeroftp-cli site --json` prints.
    pub fn report(&self) -> SiteCommandReport {
        let (code, lines, encoding, elapsed, session_reset, session_reconnected) =
            match &self.outcome {
                SiteOutcome::Replied {
                    reply,
                    elapsed,
                    session_reset,
                } => (
                    Some(reply.code),
                    reply.lines.clone(),
                    Some(match reply.encoding {
                        ReplyEncoding::Utf8 => "utf8",
                        ReplyEncoding::Latin1 => "latin1",
                    }),
                    Some(elapsed),
                    *session_reset,
                    None,
                ),
                SiteOutcome::NotSent(_) => (None, Vec::new(), None, None, false, None),
                SiteOutcome::Unknown {
                    elapsed,
                    session_reconnected,
                    ..
                } => (
                    None,
                    Vec::new(),
                    None,
                    Some(elapsed),
                    false,
                    Some(*session_reconnected),
                ),
            };
        SiteCommandReport {
            outcome: self.outcome.kind(),
            command: self.label.clone(),
            verb_kind: self.verb_kind,
            code,
            lines,
            encoding,
            elapsed_ms: elapsed.map(|e| u64::try_from(e.as_millis()).unwrap_or(u64::MAX)),
            session_reset,
            session_reconnected,
            reason: self.outcome.reason(),
        }
    }
}

/// The shared machine-readable form of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SiteCommandReport {
    /// `replied`, `not_sent` or `unknown`.
    pub outcome: &'static str,
    /// `SITE <VERB>`, never the arguments.
    pub command: String,
    pub verb_kind: VerbKind,
    pub code: Option<u16>,
    pub lines: Vec<String>,
    pub encoding: Option<&'static str>,
    pub elapsed_ms: Option<u64>,
    pub session_reset: bool,
    pub session_reconnected: Option<bool>,
    /// Why nothing was sent, or why the outcome is unknown.
    pub reason: Option<&'static str>,
}

/// Whether `SITE` applies to a session, and whether its control connection
/// is encrypted (`None` when that could not be asked).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SiteSessionStatus {
    pub supported: bool,
    pub encrypted: Option<bool>,
}

/// What the GUI shows before the first command: the plain-FTP warning has to
/// be right before anyone types a password, and only the provider knows
/// whether an explicit-if-available session fell back to clear text.
pub fn site_session_status(provider: &mut dyn StorageProvider) -> SiteSessionStatus {
    let concrete = crate::crypt_overlay_provider::concrete_provider_mut(provider);
    match concrete.as_any_mut().downcast_mut::<FtpProvider>() {
        Some(ftp) => SiteSessionStatus {
            supported: true,
            encrypted: Some(ftp.session_encrypted()),
        },
        None => SiteSessionStatus {
            supported: false,
            encrypted: None,
        },
    }
}

/// Send `raw` as a `SITE` command on `provider`'s session: the entry point
/// of the GUI, the TUI and the CLI. A crypt overlay is looked through, since
/// `SITE` travels on the control channel and never touches file content.
pub async fn run_site_command(
    provider: &mut dyn StorageProvider,
    raw: &str,
    opts: &SiteOptions,
) -> SiteRun {
    match SiteArgs::parse(raw) {
        Ok(args) => run_site_args(provider, &args, opts).await,
        Err(err) => SiteRun::not_sent(raw, NotSentReason::Input(err)),
    }
}

/// [`run_site_command`] for a line already validated with
/// [`SiteArgs::parse`], as the TUI does when the palette line is submitted.
pub async fn run_site_args(
    provider: &mut dyn StorageProvider,
    args: &SiteArgs,
    opts: &SiteOptions,
) -> SiteRun {
    let label = args.label();
    let verb_kind = args.verb_kind();
    let concrete = crate::crypt_overlay_provider::concrete_provider_mut(provider);
    let outcome = match concrete.as_any_mut().downcast_mut::<FtpProvider>() {
        Some(ftp) => ftp.site_command(args, opts).await,
        None => SiteOutcome::NotSent(NotSentReason::NotFtp),
    };
    SiteRun {
        label,
        verb_kind,
        outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_typed_line_loses_its_site_prefix_and_surrounding_whitespace_only() {
        for raw in ["WHO", "SITE WHO", "site who", "  SITE   WHO  ", "WHO\r\n"] {
            let args = SiteArgs::parse(raw).unwrap();
            assert_eq!(args.wire_line().to_ascii_uppercase(), "SITE WHO", "{raw:?}");
            assert_eq!(args.label(), "SITE WHO");
        }
        let args = SiteArgs::parse("SITE CHANGE example tagline \"a  b\"").unwrap();
        assert_eq!(args.wire_line(), "SITE CHANGE example tagline \"a  b\"");
        // Only one prefix goes: the server may define SITE SITE.
        assert_eq!(
            SiteArgs::parse("SITE SITE X").unwrap().wire_line(),
            "SITE SITE X"
        );
        // A verb that merely starts with SITE is not a prefix.
        assert_eq!(
            SiteArgs::parse("SITEX 1").unwrap().wire_line(),
            "SITE SITEX 1"
        );
    }

    #[test]
    fn a_line_that_would_smuggle_or_say_nothing_is_refused() {
        for raw in [
            "WHO\r\nDELE x",
            "WHO\nQUIT",
            "WHO\rQUIT",
            "USER a\0b",
            "CHANGE a\tb",
            "WHO\u{7f}",
            "WHO\u{85}X",
        ] {
            assert_eq!(
                SiteArgs::parse(raw),
                Err(SiteInputError::ControlCharacter),
                "{raw:?}"
            );
        }
        for raw in ["", "   ", "SITE", " site  ", "\r\n"] {
            assert_eq!(SiteArgs::parse(raw), Err(SiteInputError::Empty), "{raw:?}");
        }
        let at_limit = "x".repeat(MAX_SITE_LINE_BYTES - "SITE ".len());
        assert!(SiteArgs::parse(&at_limit).is_ok());
        assert_eq!(
            SiteArgs::parse(&format!("{at_limit}x")),
            Err(SiteInputError::TooLong)
        );
    }

    #[test]
    fn a_program_line_keeps_its_spaces_tabs_and_length() {
        let args = SiteArgs::verbatim("CHMOD 644  name ".to_string()).unwrap();
        assert_eq!(args.wire_line(), "SITE CHMOD 644  name ");
        let tab = SiteArgs::verbatim("CHMOD 644 a\tb".to_string()).unwrap();
        assert_eq!(tab.wire_line(), "SITE CHMOD 644 a\tb");
        let long = format!("CHMOD 644 /{}", "d/".repeat(800));
        assert!(
            SiteArgs::verbatim(long).is_ok(),
            "a long path is the server's call"
        );
        assert_eq!(
            SiteArgs::verbatim("CHMOD 644 a\0b".to_string()),
            Err(SiteInputError::ControlCharacter)
        );
        assert_eq!(
            SiteArgs::verbatim("CHMOD 644 a\r\nDELE b".to_string()),
            Err(SiteInputError::ControlCharacter)
        );
    }

    #[test]
    fn the_label_never_carries_an_argument_or_a_password_typed_as_a_verb() {
        let args = SiteArgs::parse("CHPASS example hunter2").unwrap();
        assert_eq!(args.label(), "SITE CHPASS");
        assert!(!format!("{args:?}").contains("hunter2"));
        assert_eq!(SiteArgs::parse("p@ss-w0rd! x").unwrap().label(), "SITE");
        assert_eq!(
            SiteArgs::parse("averyveryverylongword").unwrap().label(),
            "SITE"
        );
    }

    #[test]
    fn verbs_are_classified_for_warnings_and_refresh() {
        assert_eq!(
            SiteArgs::parse("user x").unwrap().verb_kind(),
            VerbKind::ReadOnly
        );
        assert_eq!(
            SiteArgs::parse("COLOR").unwrap().verb_kind(),
            VerbKind::SessionState
        );
        assert_eq!(
            SiteArgs::parse("CHANGE x ratio 5").unwrap().verb_kind(),
            VerbKind::Other
        );
        assert_eq!(
            SiteArgs::parse("MYSCRIPT").unwrap().verb_kind(),
            VerbKind::Other
        );
    }

    #[test]
    fn a_glftpd_box_reply_keeps_every_line_in_order() {
        let body = b"200- User Comment: Added by siteop\r\n200- +====+\r\n200- | Username: example |\r\n200-\r\n200 Command Successful.\r\n";
        let reply = SiteReply::from_body(body).unwrap();
        assert_eq!(reply.code, 200);
        assert_eq!(reply.encoding, ReplyEncoding::Utf8);
        assert_eq!(
            reply.lines,
            vec![
                "200- User Comment: Added by siteop",
                "200- +====+",
                "200- | Username: example |",
                "200-",
                "200 Command Successful.",
            ]
        );
    }

    #[test]
    fn the_code_comes_from_the_text_so_unlisted_codes_survive() {
        let reply = SiteReply::from_body(b"299 Odd but final\r\n").unwrap();
        assert_eq!(reply.code, 299);
        let reply = SiteReply::from_body(b"150 Working\r\n200 Done\r\n").unwrap();
        assert_eq!(reply.code, 200);
        assert_eq!(SiteReply::from_body(b"no code here\r\n"), None);
    }

    #[test]
    fn a_latin1_reply_is_decoded_without_losing_bytes() {
        let reply = SiteReply::from_body(b"200 Caf\xe9 \xb3\r\n").unwrap();
        assert_eq!(reply.encoding, ReplyEncoding::Latin1);
        assert_eq!(reply.lines, vec!["200 Caf\u{e9} \u{b3}"]);
    }

    #[test]
    fn reply_codes_and_colours_can_be_removed_for_reading() {
        assert_eq!(
            without_reply_code("200- | Username: x |"),
            " | Username: x |"
        );
        assert_eq!(
            without_reply_code("200 Command Successful."),
            "Command Successful."
        );
        assert_eq!(without_reply_code("200-"), "");
        assert_eq!(without_reply_code("200"), "");
        assert_eq!(
            without_reply_code(" continuation text"),
            " continuation text"
        );
        assert_eq!(without_reply_code("12 users"), "12 users");
        assert_eq!(strip_ansi("\u{1b}[1;31m200\u{1b}[0m- red"), "200- red");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn the_reply_wait_is_clamped() {
        assert_eq!(
            SiteOptions::default().reply_timeout,
            Duration::from_secs(60)
        );
        assert_eq!(
            SiteOptions::with_reply_timeout_secs(0).reply_timeout,
            MIN_REPLY_TIMEOUT
        );
        assert_eq!(
            SiteOptions::with_reply_timeout_secs(86_400).reply_timeout,
            MAX_REPLY_TIMEOUT
        );
    }

    #[test]
    fn the_report_of_a_refusal_names_the_reason_and_carries_no_input() {
        let run = SiteRun::not_sent("CHPASS example hunter2", NotSentReason::Busy);
        let json = serde_json::to_string(&run.report()).unwrap();
        assert!(json.contains("\"outcome\":\"not_sent\""), "{json}");
        assert!(json.contains("\"reason\":\"busy\""), "{json}");
        assert!(json.contains("\"command\":\"SITE CHPASS\""), "{json}");
        assert!(!json.contains("hunter2"), "{json}");
    }
}
