// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Import server profiles from an OpenSSH client config (`~/.ssh/config`).
//!
//! Parses `Host` blocks (HostName / Port / User / IdentityFile / ProxyJump),
//! maps each concrete alias to an AeroFTP `sftp` profile, and references the
//! private key by path only. Wildcard host patterns (`Host *`, any alias with
//! `*` or `?`) are skipped because they are match templates, not endpoints.
//!
//! Secret policy for this source:
//!
//! | Source state                | credential | has_stored_credential | note                       |
//! |-----------------------------|------------|-----------------------|----------------------------|
//! | SSH key referenced (file)   | None       | Some(false)           | options.private_key_path   |
//! | no key (agent / password)   | None       | Some(false)           | re-enter or use ssh-agent  |
//!
//! No key bytes are ever read or copied: only the (tilde-expanded) file path
//! is recorded. The ssh config never carries a secret, and the exporter never
//! writes one.

use crate::profile_export::ServerProfileExport;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ============ Tilde expansion ============

/// Expand a leading `~/` against `HOME` (unix/macOS) or `USERPROFILE`
/// (Windows). Any other form (absolute path, bare `~user`) is returned
/// unchanged: ssh itself only special-cases `~/` here.
fn shellexpand_tilde(p: &str) -> String {
    if let Some(rest) = p.strip_prefix("~/") {
        if let Ok(h) = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")) {
            let h = h.trim_end_matches('/');
            return format!("{h}/{rest}");
        }
    }
    p.to_string()
}

/// The environment variables an imported `${VAR}` may read: the local user
/// name and home folder, as `%u` and `%d` do.
const SSH_IMPORT_ENV: [&str; 5] = ["HOME", "USER", "USERNAME", "USERPROFILE", "LOGNAME"];

/// The values ssh substitutes for `%` tokens in one `Host` block.
struct SshTokens<'a> {
    /// `%n`: the alias the block was looked up by.
    alias: &'a str,
    /// `%h`: the host name (the alias itself inside `HostName`).
    host: &'a str,
    /// `%r`: the remote user name.
    user: &'a str,
    /// `%p`: the port.
    port: u32,
}

/// A value read from an ssh config, with its `%` tokens and, where ssh reads
/// them (`dollar`), its `${VAR}` references expanded the way ssh does, so the
/// profile holds the literal value ssh would use. `allowed` lists the tokens
/// this keyword takes (measured with `ssh -G` and `ssh -v` on 10.2p1:
/// `HostName` only `%h`; `User` also reads `%d`, `%n`, `%p` and `%u`, but
/// not `%r`). A token AeroFTP cannot resolve here, such as `%C` or `%l`, is
/// an error, and the host is skipped with it rather than imported with a
/// value no client reads as written.
///
/// The file being imported is not trusted, so `${VAR}` reads only the
/// variables in [`SSH_IMPORT_ENV`], which name the local user and home
/// folder. Any other name (`${AWS_SECRET_ACCESS_KEY}` in a `User` line would
/// hand that secret to the profile, the window and the server) skips the
/// host with the variable's name, and its value is never read.
fn ssh_expand(
    field: &str,
    value: &str,
    allowed: &str,
    tokens: &SshTokens,
    dollar: bool,
) -> Result<String, String> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let mut out = String::new();
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && dollar && chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            let closed = loop {
                match chars.next() {
                    Some('}') => break true,
                    Some(c) => name.push(c),
                    None => break false,
                }
            };
            if !closed {
                return Err(format!("its {field} holds a `${{` that is never closed"));
            }
            if !SSH_IMPORT_ENV.contains(&name.as_str()) {
                return Err(format!(
                    "its {field} reads the environment variable `{name}`, which an import does not read; only {} are expanded",
                    SSH_IMPORT_ENV.join(", ")
                ));
            }
            let found = env(&name).ok_or_else(|| {
                format!("its {field} reads the environment variable `{name}`, which is not set")
            })?;
            out.push_str(&found);
            continue;
        }
        if c != '%' {
            out.push(c);
            continue;
        }
        let token = chars.next();
        let expanded = match token {
            Some('%') => Some("%".to_string()),
            Some(t) if allowed.contains(t) => match t {
                'h' => Some(tokens.host.to_string()),
                'n' => Some(tokens.alias.to_string()),
                'r' => Some(tokens.user.to_string()),
                'p' => Some(tokens.port.to_string()),
                'u' => env("USER").or_else(|| env("USERNAME")),
                'd' => env("HOME").or_else(|| env("USERPROFILE")),
                _ => None,
            },
            _ => None,
        };
        let expanded = expanded.ok_or_else(|| {
            format!(
                "its {field} uses the ssh token %{}, which AeroFTP cannot resolve",
                token.map(String::from).unwrap_or_default()
            )
        })?;
        out.push_str(&expanded);
    }
    Ok(out)
}

/// `true` if an alias is an ssh match pattern rather than a concrete host.
fn is_wildcard_pattern(alias: &str) -> bool {
    alias.contains('*') || alias.contains('?')
}

// ============ Default config path detection ============

/// Returns the default OpenSSH client config path for the current platform.
///
/// ssh has no standard environment variable for the user config location
/// (it is always `~/.ssh/config` unless overridden per-invocation with
/// `ssh -F`), so detection is purely the per-OS default. Callers that need
/// a different file pass it explicitly to [`import_ssh_config`].
pub fn default_ssh_config_path() -> Option<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let path = PathBuf::from(home).join(".ssh").join("config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            let path = PathBuf::from(profile).join(".ssh").join("config");
            if path.exists() {
                return Some(path);
            }
        }
    }

    None
}

// ============ Parser ============

/// One parsed `Host` block: the alias list plus its lowercased key/value
/// directives. `Host` may name several patterns; the first concrete token
/// is used as the profile name.
struct HostBlock {
    aliases: Vec<String>,
    kv: HashMap<String, String>,
}

/// One ssh_config line as OpenSSH reads it (`process_config_line` in
/// readconf.c and `argv_split` in misc.c, measured with `ssh -G` on 10.2p1):
/// the keyword ends at whitespace or `=`, one `=` may follow it, and the rest
/// splits into words. Outside quotes whitespace ends a word; `"` or `'` quote
/// a part of one; `\\`, `\"` and `\'` (and `\ ` outside quotes) stand for the
/// character after the backslash, any other backslash for itself; a word
/// that starts with `#` ends the line. `Err` for a quote left open, which
/// makes ssh refuse the whole file. `ProxyJump` is read differently: its
/// value is the rest of the line up to the first `#`, quotes and spaces
/// included (`ssh -G`: `ProxyJump a b` reads as `a b`, `ProxyJump a#b` as
/// `a`), and comes back as one word.
fn split_directive(line: &str) -> Option<Result<(String, Vec<String>), String>> {
    let line = line.trim();
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (key, rest) = line.split_at(end);
    if key.is_empty() {
        return None;
    }
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest);
    let key = key.to_lowercase();
    if key == "proxyjump" {
        // ssh splits the line into words first (`argv_split`), so a quote
        // left open is refused here too: `ProxyJump "j` gives "invalid
        // quotes", while a quote inside a comment (`ProxyJump j # "x`) does not.
        if let Err(reason) = ssh_words(rest) {
            return Some(Err(reason));
        }
        let jump = rest.split('#').next().unwrap_or("").trim();
        return Some(Ok((key, vec![jump.to_string()])));
    }
    Some(ssh_words(rest).map(|words| (key, words)))
}

/// The words of an ssh_config line after its keyword; see [`split_directive`].
fn ssh_words(s: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut chars = s.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if matches!(chars.peek(), None | Some('#')) {
            return Ok(words);
        }
        let mut word = String::new();
        let mut quote = None;
        while let Some(c) = chars.next() {
            match c {
                '\\' => match chars.peek().copied() {
                    Some(next @ ('\\' | '\'' | '"')) => {
                        word.push(next);
                        chars.next();
                    }
                    Some(' ') if quote.is_none() => {
                        word.push(' ');
                        chars.next();
                    }
                    _ => word.push('\\'),
                },
                ' ' | '\t' if quote.is_none() => break,
                '"' | '\'' if quote.is_none() => quote = Some(c),
                c if quote == Some(c) => quote = None,
                c => word.push(c),
            }
        }
        if quote.is_some() {
            return Err("leaves a quote open".to_string());
        }
        words.push(word);
    }
}

/// The keywords the import reads that take one word (`ProxyJump` takes the
/// rest of its line, see [`split_directive`]).
const IMPORTED_KEYS: [&str; 4] = ["hostname", "port", "user", "identityfile"];

/// Why ssh refuses a whole file for this line, for the keywords this import
/// reads: a quote left open, a second value, no value, or a port written in
/// digits that is not from 1 to 65535 (`ssh -G` on 10.2p1: "invalid quotes",
/// "garbage at end of line", "Missing argument", "Bad port"). A port that is
/// not all digits is a service name to ssh (`Port ssh` is 22, `Port http`
/// 80, `Port Ssh` a "Bad port"), looked up in the system's services
/// database, which this import does not read: [`map_host_block`] skips only
/// that host. ssh checks every line,
/// before the first `Host` and inside a `Match` block too, and then reads
/// none of the hosts. A keyword this import does not read is left to ssh:
/// which ones exist, and how many values each takes, depends on its version.
fn ssh_line_error(key: &str, words: &[String]) -> Option<String> {
    let reads = IMPORTED_KEYS.contains(&key) || key == "proxyjump" || key == "host";
    let first = words.first().map(String::as_str).unwrap_or("");
    if !reads {
        None
    } else if first.is_empty() && words.len() <= 1 {
        Some(format!("gives `{key}` no value"))
    } else if words.len() > 1 && IMPORTED_KEYS.contains(&key) {
        Some(format!("gives `{key}` more than one value"))
    } else if key == "port"
        && port_digits(first).is_some_and(|p| p.parse::<u16>().map_or(true, |p| p == 0))
    {
        Some(format!(
            "gives `port` the value `{first}`, which is not a port from 1 to 65535"
        ))
    } else {
        None
    }
}

/// The digits of a port written as a number, with the `+` ssh accepts in
/// front (`Port +22` is 22), or `None` for anything else.
fn port_digits(value: &str) -> Option<&str> {
    let digits = value.strip_prefix('+').unwrap_or(value);
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}

/// Maximum number of `Host` blocks to parse, matching the other importers:
/// each imported profile with a credential costs a whole-vault rewrite, so an
/// uncapped parse of a large config is a self-inflicted write storm.
const MAX_HOSTS: usize = 10_000;

/// Parse the ssh config into ordered `Host` blocks. Comments (`#`) and blank
/// lines are ignored. Directives before the first `Host` (rare global block)
/// are discarded since they cannot produce a concrete profile. The second
/// value is the first line for which ssh refuses the whole file
/// ([`ssh_line_error`], or a quote left open), wherever it sits.
fn parse_ssh_config(content: &str) -> (Vec<HostBlock>, Option<String>) {
    let mut blocks: Vec<HostBlock> = Vec::new();
    let mut current: Option<HostBlock> = None;
    let mut error: Option<String> = None;
    let mut capped = false;

    for (index, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let Some(parsed) = split_directive(line) else {
            continue;
        };
        // ssh refuses the whole file for one such line, wherever it is, so
        // it is recorded for the file and every host is skipped with it.
        let line_error = match &parsed {
            Ok((key, words)) => ssh_line_error(key, words),
            Err(reason) => Some(reason.clone()),
        };
        if let Some(reason) = line_error {
            error.get_or_insert(format!(
                "line {} of the file {reason}, so ssh refuses the whole file and reads none of its hosts",
                index + 1
            ));
        }
        let Ok((key, words)) = parsed else {
            continue;
        };

        if key == "match" {
            // A `Match` block ends the `Host` block before it, and what it
            // sets applies on its own conditions: none of it is this host's.
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            continue;
        }
        if key == "host" {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            // Past the cap the lines are still read for an error, which
            // would make ssh refuse the hosts already taken.
            if blocks.len() >= MAX_HOSTS {
                if !capped {
                    log::warn!("ssh config import: stopped at the {MAX_HOSTS}-host cap");
                }
                capped = true;
                continue;
            }
            let aliases: Vec<String> = words.into_iter().filter(|s| !s.is_empty()).collect();
            current = Some(HostBlock {
                aliases,
                kv: HashMap::new(),
            });
        } else if let Some(block) = current.as_mut() {
            let value = words.into_iter().next().unwrap_or_default();
            // First occurrence wins, matching ssh's own precedence.
            block.kv.entry(key).or_insert(value);
        }
    }

    if let Some(block) = current.take() {
        blocks.push(block);
    }

    (blocks, error)
}

/// Convert one `Host` block to a profile, or `None` (with a skip reason) when
/// it is a wildcard pattern or has no usable alias.
fn map_host_block(block: &HostBlock) -> Result<ServerProfileExport, String> {
    let alias = block
        .aliases
        .iter()
        .find(|a| !a.is_empty())
        .ok_or_else(|| "empty host alias".to_string())?
        .clone();

    if block.aliases.iter().any(|a| is_wildcard_pattern(a)) {
        return Err("wildcard host pattern".to_string());
    }

    let kv = &block.kv;
    // A port in digits outside 1 to 65535 already refused the whole file; a
    // service name is skipped, since ssh resolves it from a database this
    // import does not read, and taking 22 for it would be a guess.
    let port = match kv.get("port") {
        Some(p) => p.parse::<u16>().map(u32::from).map_err(|_| {
            format!("its port `{p}` is not a number, and a service name is not looked up here; write the port number and import again")
        })?,
        None => 22,
    };
    // ssh expands `HostName` first (its `%h` is the alias), then the others
    // against that host name: the order the tokens are resolved in below.
    let mut tokens = SshTokens {
        alias: &alias,
        host: &alias,
        user: "",
        port,
    };
    let host = match kv.get("hostname").filter(|h| !h.is_empty()) {
        Some(h) => ssh_expand("host", h, "h", &tokens, false)?,
        None => alias.clone(),
    };
    tokens.host = &host;
    let username = match kv.get("user") {
        Some(u) => ssh_expand("user name", u, "dhnpu", &tokens, true)?,
        None => String::new(),
    };
    tokens.user = &username;

    let mut options = serde_json::Map::new();
    if let Some(identity) = kv.get("identityfile").filter(|s| !s.is_empty()) {
        let identity = ssh_expand("key file path", identity, "dhnpru", &tokens, true)?;
        options.insert(
            "private_key_path".into(),
            serde_json::Value::String(shellexpand_tilde(&identity)),
        );
    }
    if let Some(proxy) = kv.get("proxyjump").filter(|s| !s.is_empty()) {
        // ssh builds a `ProxyCommand` from the jump host and expands that.
        let proxy = ssh_expand("jump host", proxy, "hnpr", &tokens, false)?;
        options.insert("proxyJump".into(), serde_json::Value::String(proxy));
    }

    Ok(ServerProfileExport {
        id: format!(
            "ssh-{}-{}",
            alias.to_lowercase().replace(' ', "-"),
            &crate::bridge_shared::uuid_v4()[..8]
        ),
        name: alias,
        host,
        port,
        username,
        protocol: Some("sftp".to_string()),
        initial_path: None,
        local_initial_path: None,
        color: None,
        last_connected: None,
        options: if options.is_empty() {
            None
        } else {
            Some(serde_json::Value::Object(options))
        },
        provider_id: None,
        // The key lives on disk and is referenced by path only: no secret is
        // ever transported through the ssh config.
        credential: None,
        has_stored_credential: Some(false),
        public_url_base: None,
        ..Default::default()
    })
}

// ============ Public API ============

/// A `Host` block that was skipped (wildcard pattern or unusable alias).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshSkippedRemote {
    pub name: String,
    pub kind: String,
    pub reason: String,
}

/// Result of importing an ssh config.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SshImportResult {
    pub servers: Vec<ServerProfileExport>,
    pub skipped: Vec<SshSkippedRemote>,
    pub source_path: String,
    pub total_remotes: usize,
}

/// Import all concrete `Host` blocks from an OpenSSH client config file.
pub fn import_ssh_config(path: &Path) -> Result<SshImportResult, String> {
    let content = std::fs::read_to_string(path).map_err(|e| format!("read ssh config: {e}"))?;

    let (blocks, error) = parse_ssh_config(&content);
    let total_remotes = blocks.len();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();

    for block in &blocks {
        let display_name = block.aliases.first().cloned().unwrap_or_default();
        let mapped = match &error {
            Some(reason) => Err(reason.clone()),
            None => map_host_block(block),
        };
        match mapped {
            Ok(profile) => servers.push(profile),
            Err(reason) => skipped.push(SshSkippedRemote {
                name: display_name,
                kind: "host".to_string(),
                reason,
            }),
        }
    }

    Ok(SshImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

// ============ Export to ssh config ============

/// A server profile to export as an ssh config `Host` block.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshExportServer {
    /// The profile's id: the key of its secret in the passwords map
    /// ([`crate::bridge_shared::export_secret`]).
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub protocol: Option<String>,
    pub options: Option<serde_json::Value>,
    pub provider_id: Option<String>,
    pub initial_path: Option<String>,
}

/// Export `sftp` profiles as OpenSSH `Host` blocks.
///
/// Only `sftp` profiles map onto ssh; every other protocol is skipped. The
/// ssh config format has no password field, so the `passwords` map is
/// intentionally unused: secrets are never written. An `IdentityFile` line is
/// emitted only when the profile already references a key by path.
/// ssh `Host` patterns are whitespace separated, so an alias with a space
/// would silently become two patterns. Collapse to a safe token. The
/// characters `ssh` refuses in a destination (`'`, `"`, `` ` ``, `$`, `\`,
/// `;`, `&`, `<`, `>`, `|`, `(`, `)`, `{`, `}`, `,`: "hostname contains
/// invalid characters") become `-` too: such an alias could never be used,
/// and a quote on the `Host` line makes ssh refuse the whole file ("invalid
/// quotes"), every other host in it included. So do the characters a
/// `Host` line reads as a pattern or `ssh` reads as a user: `*` and `?` are
/// wildcards (`Host prod*` would hand this profile's host, user and key to
/// `prodfoo`), `!` negates the pattern, and `ssh a@b` connects to `b` as
/// user `a`. A leading `-` is dropped, so a leading `!` is too: `ssh -work`
/// reads it as an option. So are a leading `=`, which ssh reads as the
/// separator after `Host` (`Host =prod` is `Host prod`, and would take that
/// name from a profile called `prod`), and a leading `#`, which starts a
/// comment and leaves the block matching no host. A leading `ssh://` makes
/// `ssh` read the alias as a URI and look up the host after it, so it
/// becomes `ssh-`.
fn ssh_host_alias(name: &str) -> String {
    let alias = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() || "'\"`$\\;&<>|(){},*?!@".contains(c) {
                '-'
            } else {
                c
            }
        })
        .collect::<String>();
    let alias = alias.trim_start_matches(['-', '=', '#']);
    match alias.strip_prefix("ssh://") {
        Some(rest) => format!("ssh-{rest}"),
        None => alias.to_string(),
    }
}

/// Whether `value` holds a backslash that OpenSSH (`argv_split` in misc.c)
/// reads as an escape, and drops: one before another backslash, a quote or
/// an apostrophe, inside double quotes too, or the last one of a quoted
/// value, before its closing quote.
fn ssh_escapes(value: &str, quoted: bool) -> bool {
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        let escapes = match chars.peek() {
            None => quoted,
            Some(next) => matches!(next, '\\' | '\'' | '"'),
        };
        if c == '\\' && escapes {
            return true;
        }
    }
    false
}

/// One word of an ssh_config line (`HostName`, `User`, `ProxyJump`): a line
/// break would start keywords of its own, a `ProxyCommand` or a
/// `LocalCommand` ssh runs among them, and whitespace would end the value
/// early, so the rest is read as more arguments. A quote or an apostrophe
/// opens a quoted part, and one left open makes ssh refuse the whole file
/// ("invalid quotes"); quoting does not help here (a quoted `ProxyJump`
/// keeps its quotes), so such a value is refused, as is a backslash ssh would
/// read as an escape ([`ssh_escapes`]). So is a value that starts with `#`,
/// which ssh reads as a comment, leaving the keyword without its argument
/// and the whole file refused ("Missing argument"), or with `=`, which ssh
/// reads as the separator after the keyword and drops (`User =ops` is
/// `User ops`). A `%` is written as `%%`, which ssh reads as one `%`: unescaped,
/// ssh would expand it as a token (`ssh -G`: `HostName fe80::1%eth0` fails
/// with "unknown key %e", `User u%hx` reads as `u<host>x`; a jump host goes
/// into a `ProxyCommand`, which ssh expands too). Where ssh also expands
/// `${VAR}` (`dollar`: `User`), a value holding `${` is refused, since ssh
/// has no escape for it. A `User` holding `%` is refused too: OpenSSH reads
/// `%` tokens there only from 10.0 (its release notes: "allow %-token and
/// environment variable expansion in the ssh_config User directive"), and
/// earlier versions read `%%` as two characters, so no spelling reads the
/// same on both (`ssh -G` gives `user u%%hx` on the 9.6 of Ubuntu 24.04 and
/// `user u%hx` on 10.2).
fn ssh_word(field: &str, value: &str, dollar: bool) -> Result<String, String> {
    crate::bridge_shared::export_line_value(field, value)?;
    if value.contains(char::is_whitespace) || value.contains(['"', '\'']) {
        return Err(format!(
            "its {field} holds whitespace, a quote or an apostrophe, which ssh would read as more than one value or an open quote; correct the profile and export again"
        ));
    }
    if value.starts_with(['#', '=']) {
        return Err(format!(
            "its {field} starts with `#` or `=`, which ssh would read as a comment or as the separator after the keyword; correct the profile and export again"
        ));
    }
    if ssh_escapes(value, false) {
        return Err(format!(
            "its {field} holds a backslash before a backslash, which ssh would read as an escape; correct the profile and export again"
        ));
    }
    ssh_no_dollar(field, value, dollar)?;
    if dollar && value.contains('%') {
        return Err(format!(
            "its {field} holds `%`, which OpenSSH reads as a token from version 10.0 and as itself before, so no spelling reads the same on both; correct the profile and export again"
        ));
    }
    Ok(value.replace('%', "%%"))
}

/// Refuses a value holding `${` for a keyword where ssh expands `${VAR}`
/// (`User`, `IdentityFile`; `ssh -v`: `IdentityFile /k/${HOME}` loads
/// `/k/<home>`), since ssh_config has no escape for it.
fn ssh_no_dollar(field: &str, value: &str, dollar: bool) -> Result<(), String> {
    if dollar && value.contains("${") {
        return Err(format!(
            "its {field} holds `${{`, which ssh would expand as an environment variable; correct the profile and export again"
        ));
    }
    Ok(())
}

/// An `IdentityFile` path as ssh_config reads it: one with whitespace is
/// written in double quotes (a path may hold spaces). One with a double
/// quote, or with a backslash before another backslash, a quote or an
/// apostrophe, or at the end of a quoted path, is refused. OpenSSH
/// (`argv_split` in misc.c) reads such a backslash as an escape, inside
/// double quotes too (`ssh -G` on 10.2p1: `\\nas` reads as `\nas`, `id\'b` as
/// `id'b`, `"a\"b"` as `a"b`, and a last one before the closing quote gives
/// "invalid quotes"), so these paths could be written escaped. The export
/// refuses them instead, as a choice: a reader of the file without that
/// escape rule would take the doubled backslashes as written. Any other
/// backslash (`C:\Users\Me`, an unquoted `/keys/id\`)
/// is read as written. A path that starts with `#` or `=` is quoted too:
/// unquoted, ssh would read a comment or a separator there (see
/// [`ssh_word`]), and `"#id"` reads as `#id`. ssh expands `%` tokens and
/// `${VAR}` in a key path when it loads the key (`ssh -v`: `%u` loads the
/// local user name, `%%` one `%`), so a `%` is written as `%%`, and a path
/// holding `${` is refused ([`ssh_no_dollar`]).
fn ssh_path(field: &str, value: &str) -> Result<String, String> {
    crate::bridge_shared::export_line_value(field, value)?;
    if value.contains('"') {
        return Err(format!(
            "its {field} holds a double quote, which this export does not write, since ssh would read it only as an escape; rename the file and export again"
        ));
    }
    // An apostrophe outside quotes opens a quoted part ssh never sees closed;
    // inside double quotes it is a letter. Only a quoted path has a closing
    // quote for a last backslash to escape.
    let quoted = value.contains(char::is_whitespace)
        || value.contains('\'')
        || value.starts_with(['#', '=']);
    if ssh_escapes(value, quoted) {
        return Err(format!(
            "its {field} holds a backslash before a backslash, a quote or its end, which ssh would read as an escape; move the key and export again"
        ));
    }
    ssh_no_dollar(field, value, true)?;
    let value = value.replace('%', "%%");
    Ok(if quoted {
        format!("\"{value}\"")
    } else {
        value
    })
}

/// The lines of `server`'s `Host` block under its `Host` line, or why they
/// cannot be written. Every string is checked before any of it is written.
fn ssh_host_lines(server: &SshExportServer) -> Result<String, String> {
    let raw = |key: &str| {
        server
            .options
            .as_ref()
            .and_then(|o| o.get(key))
            .and_then(|v| v.as_str())
    };
    // The values as the profile holds them, before the trim below: a line
    // break at either end would be trimmed away, and the profile exported
    // with another value than it holds.
    for (field, value) in [
        ("host", Some(server.host.as_str())),
        ("user name", Some(server.username.as_str())),
        ("key file path", raw("private_key_path")),
        ("jump host", raw("proxyJump")),
    ] {
        crate::bridge_shared::export_line_value(field, value.unwrap_or(""))?;
    }
    let opt = |key: &str| raw(key).map(str::trim).filter(|s| !s.is_empty());
    let host = Some(server.host.trim()).filter(|s| !s.is_empty());
    let user = Some(server.username.trim()).filter(|s| !s.is_empty());
    let host = host.map(|h| ssh_word("host", h, false)).transpose()?;
    let user = user.map(|u| ssh_word("user name", u, true)).transpose()?;
    let key_file = opt("private_key_path")
        .map(|k| ssh_path("key file path", k))
        .transpose()?;
    // `ProxyJump` alone also ends its value at a `#` inside it (`ssh -G`:
    // `ProxyJump a#b` reads as `a`).
    let proxy = opt("proxyJump")
        .map(|p| match ssh_word("jump host", p, false)? {
            p if p.contains('#') => Err(
                "its jump host holds `#`, which ssh would read as the start of a comment; correct the profile and export again"
                    .to_string(),
            ),
            p => Ok(p),
        })
        .transpose()?;

    let mut block = String::new();
    if let Some(host) = host {
        block.push_str(&format!("  HostName {host}\n"));
    }
    block.push_str(&format!("  Port {}\n", server.port));
    if let Some(user) = user {
        block.push_str(&format!("  User {user}\n"));
    }
    if let Some(key_file) = key_file {
        block.push_str(&format!("  IdentityFile {key_file}\n"));
    }
    if let Some(proxy) = proxy {
        block.push_str(&format!("  ProxyJump {proxy}\n"));
    }
    block.push('\n');
    Ok(block)
}

pub fn export_ssh_config(
    servers: &[SshExportServer],
    _passwords: &HashMap<String, String>,
    out: &Path,
) -> Result<crate::bridge_shared::BridgeExportOutcome, String> {
    let mut output = String::new();
    output.push_str("# Generated by AeroFTP - https://aeroftp.app\n");
    output.push_str(&format!(
        "# Exported: {}\n\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    ));

    let mut outcome = crate::bridge_shared::BridgeExportOutcome::default();
    // ssh takes the first `Host` that matches: a second one of an alias is
    // never reached by it.
    let mut names = crate::bridge_shared::ExportEntryNames::exact(
        servers.iter().map(|s| ssh_host_alias(&s.name)),
    );

    for server in servers {
        if server.protocol.as_deref() != Some("sftp") {
            continue;
        }
        let safe_alias = ssh_host_alias(&server.name);
        // The alias turns whitespace into `-`, line breaks included; any
        // other control character would still reach the `Host` line. A name
        // that is only whitespace or a line break has no alias at all.
        let lines = crate::bridge_shared::export_line_value("name", &server.name)
            .and_then(|()| {
                if safe_alias.is_empty() {
                    Err("its name is empty, or holds only characters ssh cannot use in a host alias, and ssh finds a host only by its name; rename the profile and export again".to_string())
                } else {
                    Ok(())
                }
            })
            .and_then(|()| ssh_host_lines(server));
        match lines {
            Ok(lines) => {
                output.push_str(&format!("Host {}\n{lines}", names.claim(&safe_alias)));
                outcome.exported += 1;
            }
            Err(reason) => outcome.skip(&server.name, reason),
        }
    }
    crate::bridge_shared::write_export(outcome.exported, out, output.as_bytes())?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"
# personal hosts
Host basic
    HostName 192.168.1.50

Host nas
    HostName nas.example.com
    Port 2222
    User admin
    IdentityFile ~/.ssh/id_ed25519_nas

Host *
    ServerAliveInterval 60

Host web-?
    HostName web.example.com
    User deploy
"#;

    #[test]
    fn test_parse_ssh_config_fixture() {
        let blocks = parse_ssh_config(FIXTURE).0;
        // basic, nas, Host *, web-?
        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].aliases, vec!["basic".to_string()]);
        assert_eq!(blocks[1].kv.get("port").map(String::as_str), Some("2222"));
        assert_eq!(blocks[1].kv.get("user").map(String::as_str), Some("admin"));
        assert_eq!(
            blocks[1].kv.get("identityfile").map(String::as_str),
            Some("~/.ssh/id_ed25519_nas")
        );
        assert_eq!(blocks[2].aliases, vec!["*".to_string()]);
    }

    #[test]
    fn test_import_skips_wildcards() {
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-ssh-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&tmp, FIXTURE).unwrap();
        let result = import_ssh_config(&tmp).expect("should parse");
        std::fs::remove_file(&tmp).ok();

        assert_eq!(result.total_remotes, 4);
        // `Host *` and `web-?` are skipped wildcard patterns.
        assert_eq!(result.servers.len(), 2);
        assert_eq!(result.skipped.len(), 2);
        for s in &result.skipped {
            assert_eq!(s.reason, "wildcard host pattern");
            assert_eq!(s.kind, "host");
        }

        let basic = result.servers.iter().find(|s| s.name == "basic").unwrap();
        assert_eq!(basic.host, "192.168.1.50");
        assert_eq!(basic.port, 22); // default
        assert_eq!(basic.protocol.as_deref(), Some("sftp"));

        let nas = result.servers.iter().find(|s| s.name == "nas").unwrap();
        assert_eq!(nas.host, "nas.example.com");
        assert_eq!(nas.port, 2222);
        assert_eq!(nas.username, "admin");
    }

    #[test]
    fn test_host_without_hostname_uses_alias() {
        let blocks = parse_ssh_config("Host fallbackhost\n  User bob\n").0;
        let profile = map_host_block(&blocks[0]).expect("should map");
        assert_eq!(profile.host, "fallbackhost");
        assert_eq!(profile.username, "bob");
    }

    #[test]
    fn test_secret_policy_never_credential() {
        let blocks = parse_ssh_config(
            "Host keyed\n  HostName h.example.com\n  IdentityFile ~/.ssh/id_rsa\n",
        )
        .0;
        let profile = map_host_block(&blocks[0]).expect("should map");
        // Key referenced by path only: never a credential.
        assert!(profile.credential.is_none());
        assert_eq!(profile.has_stored_credential, Some(false));
        let opts = profile.options.expect("options should exist");
        let key = opts
            .get("private_key_path")
            .and_then(|v| v.as_str())
            .unwrap();
        // Tilde-expanded, no key bytes.
        assert!(!key.starts_with("~/"));
        assert!(key.ends_with("/.ssh/id_rsa"));
    }

    #[test]
    fn test_roundtrip_metadata_idempotence() {
        let original = r#"
Host roundtrip
    HostName rt.example.com
    Port 2200
    User carol
    IdentityFile /home/carol/.ssh/id_rt
"#;
        let tmp_in = std::env::temp_dir().join(format!(
            "aeroftp-ssh-rt-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        std::fs::write(&tmp_in, original).unwrap();
        let imported = import_ssh_config(&tmp_in).expect("import");
        std::fs::remove_file(&tmp_in).ok();
        assert_eq!(imported.servers.len(), 1);

        let s = &imported.servers[0];
        let export_servers = vec![SshExportServer {
            id: s.id.clone(),
            name: s.name.clone(),
            host: s.host.clone(),
            port: s.port,
            username: s.username.clone(),
            protocol: s.protocol.clone(),
            options: s.options.clone(),
            provider_id: s.provider_id.clone(),
            initial_path: s.initial_path.clone(),
        }];

        let tmp_out = std::env::temp_dir().join(format!(
            "aeroftp-ssh-rt-out-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        let exported =
            export_ssh_config(&export_servers, &HashMap::new(), &tmp_out).expect("export");
        assert_eq!(exported.exported, 1);

        let reimported = import_ssh_config(&tmp_out).expect("reimport");
        std::fs::remove_file(&tmp_out).ok();
        assert_eq!(reimported.servers.len(), 1);

        let r = &reimported.servers[0];
        assert_eq!(r.name, "roundtrip");
        assert_eq!(r.host, "rt.example.com");
        assert_eq!(r.port, 2200);
        assert_eq!(r.username, "carol");
        assert_eq!(r.protocol.as_deref(), Some("sftp"));
        // private_key_path metadata preserved across the round trip.
        let key = r
            .options
            .as_ref()
            .and_then(|o| o.get("private_key_path"))
            .and_then(|v| v.as_str())
            .unwrap();
        assert_eq!(key, "/home/carol/.ssh/id_rt");
        // Still no secret.
        assert!(r.credential.is_none());
        assert_eq!(r.has_stored_credential, Some(false));
    }

    #[test]
    fn test_default_path_env_override() {
        // ssh has no dedicated env var, so the override surface is HOME
        // (unix/macOS) pointing at a temp dir that contains .ssh/config.
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        {
            let _env = crate::test_env::lock();
            let prev = std::env::var("HOME").ok();
            let dir = std::env::temp_dir().join(format!(
                "aeroftp-ssh-home-{}",
                crate::bridge_shared::uuid_v4()
            ));
            std::fs::create_dir_all(dir.join(".ssh")).unwrap();
            std::fs::write(dir.join(".ssh").join("config"), "Host x\n").unwrap();

            std::env::set_var("HOME", &dir);
            let detected = default_ssh_config_path();
            assert_eq!(detected, Some(dir.join(".ssh").join("config")));

            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    #[test]
    fn test_export_skips_non_sftp() {
        let servers = vec![
            SshExportServer {
                id: "ftp-box".to_string(),
                name: "ftp-box".to_string(),
                host: "ftp.example.com".to_string(),
                port: 21,
                username: "u".to_string(),
                protocol: Some("ftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
            SshExportServer {
                id: "ssh-box".to_string(),
                name: "ssh-box".to_string(),
                host: "ssh.example.com".to_string(),
                port: 22,
                username: "u".to_string(),
                protocol: Some("sftp".to_string()),
                options: None,
                provider_id: None,
                initial_path: None,
            },
        ];
        let tmp = std::env::temp_dir().join(format!(
            "aeroftp-ssh-exp-{}.cfg",
            crate::bridge_shared::uuid_v4()
        ));
        let n = export_ssh_config(&servers, &HashMap::new(), &tmp)
            .expect("export")
            .exported;
        let conf = std::fs::read_to_string(&tmp).unwrap();
        std::fs::remove_file(&tmp).ok();

        assert_eq!(n, 1);
        assert!(conf.contains("Host ssh-box"));
        assert!(!conf.contains("ftp-box"));
        // No secret material ever written.
        assert!(!conf.to_lowercase().contains("password"));
    }

    /// Two profiles of one name (a list saved before names were unique) were
    /// two `Host Lab` blocks, and ssh takes the first `Host` that matches:
    /// the second host was not reachable by any alias.
    #[test]
    fn two_profiles_of_one_name_export_as_two_hosts() {
        let (servers, passwords) = crate::bridge_shared::two_profiles_named_lab::<SshExportServer>(
            serde_json::json!({"host": "a.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
            serde_json::json!({"host": "b.example.com", "port": 22, "username": "u", "protocol": "sftp"}),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        assert_eq!(
            export_ssh_config(&servers, &passwords, &path)
                .unwrap()
                .exported,
            2
        );
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("Host Lab\n  HostName a.example.com\n"),
            "{body}"
        );
        assert!(
            body.contains("Host Lab-2\n  HostName b.example.com\n"),
            "{body}"
        );
    }

    /// A line break in a value started keywords of its own,
    /// and ssh runs a `ProxyCommand` or a `LocalCommand` as soon as the alias
    /// is used. Each such profile is refused by name and field; a key path
    /// with spaces is quoted, and a non-ASCII name still becomes its alias.
    #[test]
    fn a_profile_whose_value_breaks_its_line_is_refused() {
        let payload = "ProxyCommand /tmp/run-me";
        let profile =
            |name: &str, host: &str, user: &str, options: serde_json::Value| -> SshExportServer {
                serde_json::from_value(serde_json::json!({
                    "id": format!("id-{name}"), "name": name, "host": host, "port": 22,
                    "username": user, "protocol": "sftp", "options": options,
                }))
                .unwrap()
            };
        let none = serde_json::json!({});
        for brk in crate::bridge_shared::LINE_BREAKS {
            let hostile = format!("x{brk}  {payload}");
            let servers = vec![
                profile(
                    &format!("name\u{1b}{brk}"),
                    "h.example.com",
                    "u",
                    none.clone(),
                ),
                profile("host", &hostile, "u", none.clone()),
                profile("user", "h.example.com", &hostile, none.clone()),
                profile(
                    "key",
                    "h.example.com",
                    "u",
                    serde_json::json!({"private_key_path": hostile}),
                ),
                profile(
                    "jump",
                    "h.example.com",
                    "u",
                    serde_json::json!({"proxyJump": hostile}),
                ),
                profile(
                    "Büro nas",
                    "nas.example.com",
                    "élodie",
                    serde_json::json!({"private_key_path": "/home/élodie/My Keys/id_ed25519"}),
                ),
            ];
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("config");
            let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
            assert_eq!(outcome.exported, 1, "{brk:?}: {:?}", outcome.skipped);
            let fields = ["name", "host", "user name", "key file path", "jump host"];
            assert_eq!(outcome.skipped.len(), fields.len(), "{:?}", outcome.skipped);
            for (skip, (server, field)) in outcome.skipped.iter().zip(servers.iter().zip(fields)) {
                assert_eq!(skip.name, server.name);
                assert!(
                    skip.reason.starts_with(&format!("its {field} ")),
                    "{}",
                    skip.reason
                );
                assert!(!skip.reason.contains(payload));
            }
            let body = std::fs::read_to_string(&path).unwrap();
            crate::bridge_shared::assert_no_injected_line(&body, payload);
            assert!(
                body.contains("Host Büro-nas\n  HostName nas.example.com\n  Port 22\n  User élodie\n  IdentityFile \"/home/élodie/My Keys/id_ed25519\"\n"),
                "{body}"
            );
        }
        // A space ends the value as surely: refused, not cut.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let spaced = [profile("spaced", "h.example.com ProxyCommand", "u", none)];
        let outcome = export_ssh_config(&spaced, &HashMap::new(), &path).unwrap();
        assert_eq!(outcome.exported, 0);
        assert!(
            outcome.skipped[0]
                .reason
                .starts_with("its host holds whitespace"),
            "{:?}",
            outcome.skipped
        );
    }

    fn sftp(name: &str, host: &str, user: &str, options: serde_json::Value) -> SshExportServer {
        serde_json::from_value(serde_json::json!({
            "id": format!("id-{name}"), "name": name, "host": host, "port": 22,
            "username": user, "protocol": "sftp", "options": options,
        }))
        .unwrap()
    }

    /// A key file path with spaces is written in double quotes, read back
    /// without them, and exported again as the same path.
    #[test]
    fn a_quoted_key_path_survives_export_import_and_export_again() {
        let path = "/home/u/My Keys/id_ed25519";
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let servers = [sftp(
            "lab",
            "h.example.com",
            "u",
            serde_json::json!({"private_key_path": path}),
        )];
        assert_eq!(
            export_ssh_config(&servers, &HashMap::new(), &first)
                .unwrap()
                .exported,
            1
        );
        let imported = import_ssh_config(&first).unwrap();
        assert_eq!(imported.servers.len(), 1);
        let key = imported.servers[0]
            .options
            .as_ref()
            .and_then(|o| o.get("private_key_path"))
            .and_then(|v| v.as_str());
        assert_eq!(key, Some(path), "{:?}", imported.servers[0].options);
        let again = [sftp(
            "lab",
            "h.example.com",
            "u",
            serde_json::json!({"private_key_path": key.unwrap()}),
        )];
        let second = dir.path().join("second");
        let outcome = export_ssh_config(&again, &HashMap::new(), &second).unwrap();
        assert_eq!(outcome.exported, 1, "{:?}", outcome.skipped);
        // The same file but for the `# Exported:` time, which may tick
        // between the two exports.
        let body = |path: &std::path::Path| {
            std::fs::read_to_string(path)
                .unwrap()
                .lines()
                .filter(|l| !l.starts_with("# Exported: "))
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert_eq!(body(&first), body(&second));
        assert!(
            body(&first).contains("  IdentityFile \"/home/u/My Keys/id_ed25519\"\n"),
            "{}",
            body(&first)
        );
    }

    /// A line break at either end of a value is refused like one inside it
    /// (the trim must not hide it), a tab in a host or user name splits the
    /// value as a space would, and a name that is only whitespace or a line
    /// break is reported instead of dropped.
    #[test]
    fn a_break_at_the_edge_a_tab_or_an_empty_name_is_reported() {
        let none = serde_json::json!({});
        let servers = [
            sftp("lead", "\nh.example.com", "u", none.clone()),
            sftp("trail", "h.example.com", "u\r", none.clone()),
            sftp(
                "key",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/id\n"}),
            ),
            sftp(
                "jump",
                "h.example.com",
                "u",
                serde_json::json!({"proxyJump": "\r\nbastion"}),
            ),
            sftp("tab", "h.example.com", "a\tb", none.clone()),
            sftp("apostrophe user", "h.example.com", "o'brien", none.clone()),
            sftp(
                "escaped jump",
                "h.example.com",
                "u",
                serde_json::json!({"proxyJump": "a\\\\b"}),
            ),
            sftp(
                "unc",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "\\\\nas\\My Keys\\id"}),
            ),
            sftp(
                "trailing",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "C:\\My Keys\\"}),
            ),
            sftp(
                "apostrophe",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/My Keys/id\\'backup"}),
            ),
            sftp(
                "bare apostrophe",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/id\\'backup"}),
            ),
            sftp(
                "windows",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "C:\\Users\\John Doe\\.ssh\\id"}),
            ),
            sftp("hash user", "h.example.com", "#ops", none.clone()),
            sftp("equals host", "=h.example.com", "u", none.clone()),
            sftp(
                "hash jump",
                "h.example.com",
                "u",
                serde_json::json!({"proxyJump": "#bastion"}),
            ),
            sftp(
                "equals jump",
                "h.example.com",
                "u",
                serde_json::json!({"proxyJump": "=bastion"}),
            ),
            sftp(
                "inner hash jump",
                "h.example.com",
                "u",
                serde_json::json!({"proxyJump": "bastion#2"}),
            ),
            sftp("dollar user", "h.example.com", "u${HOME}", none.clone()),
            sftp(
                "dollar key",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/${HOME}/id"}),
            ),
            sftp("percent user", "h.example.com", "u%x", none.clone()),
            sftp(
                "percent",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/id%u", "proxyJump": "j%h"}),
            ),
            sftp("\n", "h.example.com", "u", none.clone()),
            sftp("   ", "h.example.com", "u", none.clone()),
            sftp("good", "h.example.com", "u", none.clone()),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
        assert_eq!(outcome.exported, 3, "{:?}", outcome.skipped);
        let want = [
            ("lead", "its host holds a line break"),
            ("trail", "its user name holds a line break"),
            ("key", "its key file path holds a line break"),
            ("jump", "its jump host holds a line break"),
            ("tab", "its user name holds whitespace"),
            (
                "apostrophe user",
                "its user name holds whitespace, a quote or an apostrophe",
            ),
            ("escaped jump", "its jump host holds a backslash"),
            ("unc", "its key file path holds a backslash before"),
            ("trailing", "its key file path holds a backslash before"),
            ("apostrophe", "its key file path holds a backslash before"),
            (
                "bare apostrophe",
                "its key file path holds a backslash before",
            ),
            ("hash user", "its user name starts with `#` or `=`"),
            ("equals host", "its host starts with `#` or `=`"),
            ("hash jump", "its jump host starts with `#` or `=`"),
            ("equals jump", "its jump host starts with `#` or `=`"),
            ("inner hash jump", "its jump host holds `#`"),
            ("dollar user", "its user name holds `${`"),
            ("dollar key", "its key file path holds `${`"),
            ("percent user", "its user name holds `%`"),
            ("\n", "its name holds a line break"),
            ("   ", "its name is empty"),
        ];
        assert_eq!(outcome.skipped.len(), want.len(), "{:?}", outcome.skipped);
        for (skip, (name, reason)) in outcome.skipped.iter().zip(want) {
            assert_eq!(skip.name, name);
            assert!(skip.reason.starts_with(reason), "{name:?}: {}", skip.reason);
        }
        let config = std::fs::read_to_string(&path).unwrap();
        assert_eq!(config.matches("Host ").count(), 3, "{config}");
        // A `%` goes out as `%%`, which ssh reads as one `%` (`ssh -v` on
        // 10.2p1, for the key it loads and the `ProxyCommand` it runs).
        assert!(config.contains("  IdentityFile /keys/id%%u\n"), "{config}");
        assert!(config.contains("  ProxyJump j%%h\n"), "{config}");
        assert!(
            config.contains("  IdentityFile \"C:\\Users\\John Doe\\.ssh\\id\"\n"),
            "{config}"
        );
    }

    /// The exported key paths as OpenSSH itself reads them (`ssh -G`), where
    /// an `ssh` is installed: each reads back as the profile holds it.
    #[test]
    fn openssh_reads_each_exported_key_path_as_written() {
        if std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!("no ssh here: the quoting is pinned by the export tests only");
            return;
        }
        let paths = [
            "/home/u/.ssh/id_ed25519",
            "/home/u/My Keys/id_ed25519",
            "C:\\Users\\John Doe\\.ssh\\id",
            "C:\\Users\\me\\.ssh\\id",
            "/keys/My Keys/id\\#b",
            "/keys/id\\",
            "/keys/id'backup",
            "#id_ed25519",
            "=id_ed25519",
        ];
        let servers: Vec<SshExportServer> = paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                sftp(
                    &format!("k{i}"),
                    "h.example.com",
                    "u",
                    serde_json::json!({"private_key_path": path}),
                )
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        let outcome = export_ssh_config(&servers, &HashMap::new(), &config).unwrap();
        assert_eq!(outcome.exported, paths.len(), "{:?}", outcome.skipped);
        for (i, path) in paths.iter().enumerate() {
            let read = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&config)
                .arg(format!("k{i}"))
                .output()
                .unwrap();
            assert!(
                read.status.success(),
                "{path}: {}",
                String::from_utf8_lossy(&read.stderr)
            );
            let stdout = String::from_utf8_lossy(&read.stdout);
            let identity: Vec<&str> = stdout
                .lines()
                .filter_map(|l| l.strip_prefix("identityfile "))
                .collect();
            assert_eq!(identity, [*path], "{stdout}");
        }
        // A name with an apostrophe or another character ssh refuses in a
        // destination: its alias is usable, and the file stays readable for
        // every host in it.
        // A name with a wildcard, a negation, an `@`, a leading `=` or `#`,
        // or an `ssh://` in front gets an alias that ssh resolves to its own
        // profile. `prod*`, `web?`, `=lab` and `ssh://box` come before the
        // host each of them would otherwise take, since ssh uses the first
        // block that matches.
        let named = [
            sftp("Bob's server", "a.example.com", "u", serde_json::json!({})),
            sftp("(work) nas", "b.example.com", "u", serde_json::json!({})),
            sftp("plain", "c.example.com", "u", serde_json::json!({})),
            sftp("prod*", "d.example.com", "u", serde_json::json!({})),
            sftp("prodfoo", "e.example.com", "u", serde_json::json!({})),
            sftp("web?", "f.example.com", "u", serde_json::json!({})),
            sftp("web1", "g.example.com", "u", serde_json::json!({})),
            sftp("!nas", "h.example.com", "u", serde_json::json!({})),
            sftp("a@b", "i.example.com", "u", serde_json::json!({})),
            sftp("=lab", "j.example.com", "u", serde_json::json!({})),
            sftp("lab", "k.example.com", "u", serde_json::json!({})),
            sftp("ssh://box", "l.example.com", "u", serde_json::json!({})),
            sftp("box", "m.example.com", "u", serde_json::json!({})),
            sftp("#tag", "n.example.com", "u", serde_json::json!({})),
            sftp("zone", "fe80::1%eth0", "u", serde_json::json!({})),
        ];
        let outcome = export_ssh_config(&named, &HashMap::new(), &config).unwrap();
        assert_eq!(outcome.exported, named.len(), "{:?}", outcome.skipped);
        let written = std::fs::read_to_string(&config).unwrap();
        let aliases: Vec<&str> = written
            .lines()
            .filter_map(|l| l.strip_prefix("Host "))
            .collect();
        assert_eq!(aliases.len(), named.len(), "{written}");
        for (alias, server) in aliases.iter().zip(&named) {
            let read = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&config)
                .arg(alias)
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&read.stdout);
            assert!(
                read.status.success()
                    && stdout
                        .lines()
                        .any(|l| l == format!("hostname {}", server.host))
                    && stdout
                        .lines()
                        .any(|l| l == format!("user {}", server.username)),
                "{} as {alias}: {stdout} {}",
                server.name,
                String::from_utf8_lossy(&read.stderr)
            );
        }
    }

    /// Whether this `ssh` expands `%` tokens in `User`, as OpenSSH does from
    /// 10.0: `User %n` reads as the alias there, and as `%n` before.
    fn ssh_expands_user_tokens(dir: &Path) -> bool {
        let probe = dir.join("probe");
        std::fs::write(&probe, "Host probe\n  User %n\n").unwrap();
        std::process::Command::new("ssh")
            .arg("-G")
            .arg("-F")
            .arg(&probe)
            .arg("probe")
            .output()
            .is_ok_and(|out| {
                String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .any(|l| l == "user probe")
            })
    }

    /// A config's `%` tokens and `${VAR}` references are imported as the
    /// values ssh uses, so the profile holds a literal value, and exporting
    /// it again gives ssh the same host and user. A token AeroFTP cannot
    /// resolve skips the host with its reason.
    #[test]
    fn an_imported_token_is_expanded_as_ssh_reads_it() {
        // The import and `ssh -G` read the environment, which other tests in
        // this crate change under this lock.
        let _env = crate::test_env::lock();
        let secret = "s3cr3t-value-never-imported";
        std::env::set_var("AEROFTP_SSH_TEST_SECRET", secret);
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(
            &source,
            "Host al\n  HostName %h.example.com\n  Port 2200\n  User %n-x\n  IdentityFile %d/id_%h_%r_%%\n  ProxyJump %r@jump.%h\n\nHost tok\n  HostName h.example.com\n  IdentityFile /keys/%C\n\nHost secret\n  HostName h.example.com\n  User ${AEROFTP_SSH_TEST_SECRET}\n\nHost open\n  HostName h.example.com\n  IdentityFile /keys/${HOME\n",
        )
        .unwrap();
        let result = import_ssh_config(&source).unwrap();
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        std::env::remove_var("AEROFTP_SSH_TEST_SECRET");
        assert_eq!(skipped.len(), 3, "{skipped:?}");
        assert_eq!(skipped[0].0, "tok");
        assert!(
            skipped[0]
                .1
                .starts_with("its key file path uses the ssh token %C"),
            "{skipped:?}"
        );
        assert_eq!(skipped[1].0, "secret");
        assert!(
            skipped[1].1.starts_with(
                "its user name reads the environment variable `AEROFTP_SSH_TEST_SECRET`, which an import does not read"
            ),
            "{skipped:?}"
        );
        assert_eq!(skipped[2].0, "open");
        assert!(
            skipped[2]
                .1
                .starts_with("its key file path holds a `${` that is never closed"),
            "{skipped:?}"
        );
        let everything = serde_json::to_string(&result.servers).unwrap()
            + &serde_json::to_string(&result.skipped).unwrap();
        assert!(!everything.contains(secret), "{everything}");
        assert_eq!(result.servers.len(), 1);
        let al = &result.servers[0];
        assert_eq!(al.host, "al.example.com");
        assert_eq!(al.username, "al-x");
        assert_eq!(al.port, 2200);
        let options = al.options.as_ref().unwrap();
        assert_eq!(
            options["private_key_path"],
            format!("{home}/id_al.example.com_al-x_%")
        );
        assert_eq!(options["proxyJump"], "al-x@jump.al.example.com");

        let mut profile = sftp(&al.name, &al.host, &al.username, options.clone());
        profile.port = al.port;
        let exported = dir.path().join("exported");
        export_ssh_config(&[profile], &HashMap::new(), &exported).unwrap();
        let written = std::fs::read_to_string(&exported).unwrap();
        assert!(
            written.contains(&format!(
                "  IdentityFile {home}/id_al.example.com_al-x_%%\n"
            )) || written.contains(&format!(
                "  IdentityFile \"{home}/id_al.example.com_al-x_%%\"\n"
            )),
            "{written}"
        );
        if std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_err()
        {
            return;
        }
        // OpenSSH expands `%` tokens in `User` only from 10.0; an earlier
        // ssh (9.6 on the Ubuntu 24.04 CI runner, and the Windows one) reads
        // `User %n-x` as written. The import reads it as 10.0 does, so the
        // user is compared only where this ssh does too.
        let user_tokens = ssh_expands_user_tokens(dir.path());
        if !user_tokens {
            eprintln!("this ssh reads `User` tokens as written (before 10.0): the user is not compared with ssh -G");
        }
        let read = |config: &Path| {
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(config)
                .arg("al")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| {
                    l.starts_with("hostname ")
                        || l.starts_with("port ")
                        || (user_tokens && l.starts_with("user "))
                })
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(read(&source), read(&exported));
    }

    /// Each line is read as OpenSSH reads it: a trailing comment, `Key=value`,
    /// escapes, an apostrophe quote, and `ProxyJump` up to its comment. Every
    /// imported value is the one `ssh -G` reports for the same file. A line
    /// ssh refuses (two values or none, a quote left open, a bad port) makes
    /// it refuse the whole file, so every host is skipped with that line,
    /// and what a `Match` block sets is not taken for the host before it.
    #[test]
    fn each_imported_value_is_what_openssh_reads() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good");
        std::fs::write(
            &good,
            "Host esc\n  HostName h.example.com # prod\n  User 'o\\'brien'\n  IdentityFile \"C:\\\\Keys\\\\my id\"\n  ProxyJump j1,j2 # two hops\n\n\
             Host eq\n  HostName=e.example.com\n  User = ops\n\n\
             Host m\n  HostName m.example.com\nMatch all\n  User matched\n\n\
             Host pj\n  HostName p.example.com\n  ProxyJump jump.example.com#x\n\n\
             Host pq\n  HostName q.example.com\n  ProxyJump j1 # \"x\n",
        )
        .unwrap();
        let result = import_ssh_config(&good).unwrap();
        assert_eq!(
            result.skipped.len(),
            0,
            "{:?}",
            result.skipped.iter().map(|s| &s.reason).collect::<Vec<_>>()
        );
        let by = |name: &str| result.servers.iter().find(|s| s.name == name).unwrap();
        assert_eq!(
            (by("esc").host.as_str(), by("esc").username.as_str()),
            ("h.example.com", "o'brien")
        );
        let esc = by("esc").options.as_ref().unwrap();
        assert_eq!(esc["private_key_path"], "C:\\Keys\\my id");
        assert_eq!(esc["proxyJump"], "j1,j2");
        assert_eq!(
            (by("eq").host.as_str(), by("eq").username.as_str()),
            ("e.example.com", "ops")
        );
        assert_eq!(by("m").username, "");
        // A quote inside the comment of a `ProxyJump` line is not read.
        assert_eq!(by("pq").options.as_ref().unwrap()["proxyJump"], "j1");
        // `ProxyJump` ends at a `#` even inside a word, unlike the others.
        assert_eq!(
            by("pj").options.as_ref().unwrap()["proxyJump"],
            "jump.example.com"
        );

        // A line ssh refuses makes it refuse the whole file, wherever the
        // line is: every host is skipped with that line's reason.
        let refused = [
            (
                "Host fine\n  HostName f.example.com\nHost two\n  HostName a b\n",
                4,
                "gives `hostname` more than one value",
            ),
            (
                "Host fine\n  HostName f.example.com\nHost open\n  User \"x\n",
                4,
                "leaves a quote open",
            ),
            (
                "User \"x\nHost fine\n  HostName f.example.com\n",
                1,
                "leaves a quote open",
            ),
            (
                "HostName a b\nHost fine\n  HostName f.example.com\n",
                1,
                "gives `hostname` more than one value",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch host zzz\n  User a b\n",
                4,
                "gives `user` more than one value",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch host zzz\n  User 'x\n",
                4,
                "leaves a quote open",
            ),
            (
                "Host fine\n  HostName f.example.com\nHost o\n  User\n",
                4,
                "gives `user` no value",
            ),
            (
                "Host fine\n  HostName f.example.com\nHost o\n  User #x\n",
                4,
                "gives `user` no value",
            ),
            (
                "Host fine\n  HostName f.example.com\nHost o\n  ProxyJump \"j\n",
                4,
                "leaves a quote open",
            ),
            (
                "Host fine\n  HostName f.example.com\nHost o\n  ProxyJump #x\n",
                4,
                "gives `proxyjump` no value",
            ),
            (
                "Host\nHost fine\n  HostName f.example.com\n",
                1,
                "gives `host` no value",
            ),
            (
                "Host fine\n  HostName f.example.com\n  Port 0\n",
                3,
                "gives `port` the value `0`",
            ),
            (
                "Host fine\n  HostName f.example.com\n  Port 65536\n",
                3,
                "gives `port` the value `65536`",
            ),
            (
                "Host fine\n  HostName f.example.com\n  Port 99999999999999999999\n",
                3,
                "gives `port` the value `99999999999999999999`",
            ),
        ];
        let bad = dir.path().join("bad");
        for (config, line, reason) in refused {
            std::fs::write(&bad, config).unwrap();
            let result = import_ssh_config(&bad).unwrap();
            assert_eq!(result.servers.len(), 0, "{config:?}");
            assert!(
                result.skipped.iter().any(|s| s.name == "fine"),
                "{config:?}"
            );
            for skip in &result.skipped {
                assert!(
                    skip.reason
                        .starts_with(&format!("line {line} of the file {reason}")),
                    "{config:?}: {}",
                    skip.reason
                );
                assert!(skip
                    .reason
                    .ends_with("so ssh refuses the whole file and reads none of its hosts"));
            }
        }
        // A service name skips only its host: `Port ssh` is port 22 to ssh,
        // but only through the system's services database.
        for name in ["ssh", "22x"] {
            std::fs::write(
                &bad,
                format!("Host fine\n  HostName f.example.com\nHost svc\n  Port {name}\n"),
            )
            .unwrap();
            let result = import_ssh_config(&bad).unwrap();
            assert_eq!(result.servers.len(), 1, "{name}");
            assert_eq!(result.servers[0].name, "fine");
            assert_eq!(result.skipped.len(), 1, "{name}");
            assert_eq!(result.skipped[0].name, "svc");
            assert!(
                result.skipped[0]
                    .reason
                    .starts_with(&format!("its port `{name}` is not a number")),
                "{}",
                result.skipped[0].reason
            );
        }
        // A port ssh reads as a number: `+22` and `022` are port 22.
        std::fs::write(
            &bad,
            "Host fine\n  HostName f.example.com\n  Port +22\nHost z\n  Port 022\n",
        )
        .unwrap();
        let result = import_ssh_config(&bad).unwrap();
        assert_eq!(
            result.servers.len(),
            2,
            "{:?}",
            result.skipped.iter().map(|s| &s.reason).collect::<Vec<_>>()
        );
        assert!(result.servers.iter().all(|s| s.port == 22));

        if std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_err()
        {
            return;
        }
        for (config, _, _) in refused {
            std::fs::write(&bad, config).unwrap();
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&bad)
                .arg("fine")
                .output()
                .unwrap();
            assert!(!out.status.success(), "ssh read {config:?}");
        }
        // `ssh -G` on the good file: the same values for each host (a host
        // without `User` gets the local user from ssh, so it is not compared).
        let read = |alias: &str| {
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&good)
                .arg(alias)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).to_string()
        };
        for alias in ["esc", "eq"] {
            let got = read(alias);
            let server = by(alias);
            assert!(
                got.lines()
                    .any(|l| l == format!("hostname {}", server.host)),
                "{got}"
            );
            assert!(
                got.lines()
                    .any(|l| l == format!("user {}", server.username)),
                "{got}"
            );
        }
        let got = read("esc");
        assert!(
            got.lines().any(|l| l == "identityfile C:\\Keys\\my id"),
            "{got}"
        );
        assert!(got.lines().any(|l| l == "proxyjump j1,j2"), "{got}");
        let got = read("pj");
        assert!(
            got.lines().any(|l| l == "proxyjump jump.example.com"),
            "{got}"
        );
    }
}
