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

/// Expand a leading `~/` against the local account's home folder, as ssh
/// does (`HOME=/tmp ssh -vvv` still loads `IdentityFile ~/k` from the
/// passwd home; only `${HOME}` reads the environment). Any other form
/// (absolute path, bare `~user`) is returned unchanged: ssh itself only
/// special-cases `~/` here.
fn shellexpand_tilde(p: &str) -> Result<String, String> {
    match p.strip_prefix("~/") {
        // ssh stops when it cannot read the home folder (`tilde_expand`).
        Some(rest) => local_account()
            .1
            .map(|h| format!("{}/{rest}", h.trim_end_matches('/')))
            .ok_or_else(|| {
                "its key file path starts with `~/`, and the local account's home folder could not be read".to_string()
            }),
        None => Ok(p.to_string()),
    }
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
    /// `%r`: the remote user name, `None` when it cannot be known.
    user: Option<&'a str>,
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
                'r' => tokens.user.map(str::to_string),
                'p' => Some(tokens.port.to_string()),
                'u' => local_account().0,
                'd' => local_account().1,
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

/// The local account's name and home folder, as ssh reads them for `%u`, `%d`
/// and a `%r` with no `User`: on Unix the passwd entry of the real user, as ssh reads it (`getuid`),
/// not `USER` or `HOME` (`ssh -G` with `USER=foo` still gives the account's
/// name); elsewhere the environment, as Win32-OpenSSH reads it.
fn local_account() -> (Option<String>, Option<String>) {
    #[cfg(unix)]
    {
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buf = vec![0 as libc::c_char; 16 * 1024];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: the buffers outlive the call, and the strings read below
        // point into `buf`, which is alive while they are copied.
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 || result.is_null() {
            return (None, None);
        }
        let field = |ptr: *const libc::c_char| {
            (!ptr.is_null())
                .then(|| {
                    unsafe { std::ffi::CStr::from_ptr(ptr) }
                        .to_str()
                        .ok()
                        .map(str::to_string)
                })
                .flatten()
                .filter(|v| !v.is_empty())
        };
        (field(pwd.pw_name), field(pwd.pw_dir))
    }
    #[cfg(not(unix))]
    {
        let env = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        (env("USERNAME"), env("USERPROFILE"))
    }
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
    // ssh reads `~/.ssh/config` from the account's passwd home, whatever
    // `HOME` says (`HOME=/tmp/x ssh -G` ignores `/tmp/x/.ssh/config`).
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        if let Some(path) = ssh_config_in(local_account().1) {
            return Some(path);
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

/// `<home>/.ssh/config`, when that file exists.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn ssh_config_in(home: Option<String>) -> Option<PathBuf> {
    let path = PathBuf::from(home?).join(".ssh").join("config");
    path.exists().then_some(path)
}

// ============ Parser ============

/// Which hosts the lines of a block apply to.
#[derive(Debug, PartialEq)]
enum Scope {
    /// The lines before the first `Host` or `Match`: every host.
    Global,
    /// A `Host` line: the hosts its patterns match ([`host_matches`]).
    Host,
    /// A `Match` line: the hosts its conditions hold for ([`match_holds`]).
    Match(MatchLine),
}

/// The conditions of one `Match` line, on the given line of the file. Each is
/// negated when its flag is set; the line holds when all of them do.
#[derive(Debug, PartialEq)]
struct MatchLine {
    line: usize,
    criteria: Vec<(bool, Criterion)>,
}

/// One `Match` condition, as ssh reads it (`match_cfg_line` in readconf.c,
/// measured with `ssh -G` on 10.2p1).
#[derive(Debug, PartialEq)]
enum Criterion {
    /// `all`: every host.
    All,
    /// `canonical`: only in the second reading of the file ([`host_values`]).
    Canonical,
    /// `final`: the same, and a line naming it (not negated) makes ssh read
    /// the file a second time (`ssh -G`: `Match originalhost zz final` does,
    /// `Match canonical` and `Match !final` do not).
    Final,
    /// `host`: the host name read so far (or the alias), any case.
    Host(String),
    /// `originalhost`: the alias, any case.
    OriginalHost(String),
    /// `user`: the `User` read so far as written (or the local account's
    /// name), case included.
    User(String),
    /// `localuser`: the local account's name, case included.
    LocalUser(String),
    /// A condition known only when ssh runs (`exec`, `localnetwork`,
    /// `tagged`, `version`, `sessiontype`, `command`).
    Runtime,
}

/// The conditions of a `Match` line, or why ssh refuses the whole file for
/// it (`ssh -G` on 10.2p1: a bare `Match`, a condition it does not know, a
/// condition with no value, and `all` followed by another condition are each
/// "Bad Match condition"). A condition may be written `name=value`.
fn parse_match(words: &[String]) -> Result<Vec<(bool, Criterion)>, String> {
    if words.is_empty() {
        return Err("gives `Match` no condition".to_string());
    }
    let mut criteria = Vec::new();
    let mut rest = words.iter();
    while let Some(word) = rest.next() {
        let (negated, word) = match word.strip_prefix('!') {
            Some(word) => (true, word),
            None => (false, word.as_str()),
        };
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (word, None),
        };
        let name = name.to_ascii_lowercase();
        let criterion = match name.as_str() {
            "all" if criteria.is_empty() && words.len() > 1 => {
                return Err("combines `Match all` with another condition".to_string())
            }
            "all" => Criterion::All,
            "canonical" => Criterion::Canonical,
            "final" => Criterion::Final,
            "host" | "originalhost" | "user" | "localuser" | "exec" | "localnetwork" | "tagged"
            | "version" | "sessiontype" | "command" => {
                let value = match inline {
                    Some(value) => Some(value.to_string()),
                    None => rest.next().cloned(),
                }
                .filter(|v| !v.is_empty())
                .ok_or_else(|| format!("gives the `Match` condition `{name}` no value"))?;
                match name.as_str() {
                    "host" => Criterion::Host(value),
                    "originalhost" => Criterion::OriginalHost(value),
                    "user" => Criterion::User(value),
                    "localuser" => Criterion::LocalUser(value),
                    _ => Criterion::Runtime,
                }
            }
            _ => {
                return Err(format!(
                    "gives `Match` the condition `{name}`, which ssh does not know"
                ))
            }
        };
        criteria.push((negated, criterion));
    }
    Ok(criteria)
}

/// One parsed block: where it applies, the `Host` patterns (empty for the
/// others), and its lowercased key/value directives, the first value of each
/// key. `Host` may name several patterns; the first concrete one is used as
/// the profile name ([`profile_alias`]).
struct HostBlock {
    scope: Scope,
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
    if key == "match" {
        return parse_match(words).err();
    }
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

/// The port of a service name, as ssh looks it up (`getservbyname` for
/// `tcp`, case included: `Port ssh` is 22, `Port Ssh` a "Bad port"), from the
/// system's services file. A name the file does not list, or a file that
/// cannot be read, skips the host: ssh may know the name from another source
/// of that database, so the file is not refused for it.
fn service_port(name: &str) -> Result<u32, String> {
    #[cfg(windows)]
    let path = std::path::PathBuf::from(
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string()),
    )
    .join(r"System32\drivers\etc\services");
    #[cfg(not(windows))]
    let path = std::path::PathBuf::from("/etc/services");
    let services = std::fs::read_to_string(&path).map_err(|e| {
        format!("its port `{name}` is a service name, and the services file could not be read ({e}); write the port number and import again")
    })?;
    services
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('#').next()?.split_whitespace();
            let service = fields.next()?;
            let (port, protocol) = fields.next()?.split_once('/')?;
            let named = service == name || fields.any(|alias| alias == name);
            (named && protocol == "tcp").then(|| port.parse::<u16>().ok())?
        })
        .find(|port| *port != 0)
        .map(u32::from)
        .ok_or_else(|| {
            format!("its port `{name}` is a service name the services file does not list; write the port number and import again")
        })
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

/// Parse the ssh config into its blocks, in file order: the lines before the
/// first `Host` or `Match` (when there are any), then one block per `Host`
/// and `Match` line. Comments (`#`) and blank lines are ignored. Past
/// [`MAX_HOSTS`] `Host` blocks no block is kept. The second value is the
/// first line for which ssh refuses the whole file ([`ssh_line_error`], or a
/// quote left open), wherever it sits.
fn parse_ssh_config(content: &str) -> (Vec<HostBlock>, Option<String>) {
    let mut blocks: Vec<HostBlock> = Vec::new();
    let mut current: Option<HostBlock> = Some(HostBlock {
        scope: Scope::Global,
        aliases: Vec::new(),
        kv: HashMap::new(),
    });
    let mut error: Option<String> = None;
    let mut hosts = 0;
    let mut capped = false;
    let push = |blocks: &mut Vec<HostBlock>, block: HostBlock| {
        if block.scope != Scope::Global || !block.kv.is_empty() {
            blocks.push(block);
        }
    };

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

        if key == "host" || key == "match" {
            if let Some(block) = current.take() {
                push(&mut blocks, block);
            }
            // Past the cap the lines are still read for an error, which
            // would make ssh refuse the hosts already taken.
            if hosts >= MAX_HOSTS {
                if !capped {
                    log::warn!("ssh config import: stopped at the {MAX_HOSTS}-host cap");
                }
                capped = true;
                continue;
            }
            let scope = match key.as_str() {
                "host" => {
                    hosts += 1;
                    Scope::Host
                }
                // A line ssh refuses has refused the whole file above.
                _ => Scope::Match(MatchLine {
                    line: index + 1,
                    criteria: parse_match(&words).unwrap_or_default(),
                }),
            };
            let aliases = match scope {
                Scope::Host => words.into_iter().filter(|s| !s.is_empty()).collect(),
                _ => Vec::new(),
            };
            current = Some(HostBlock {
                scope,
                aliases,
                kv: HashMap::new(),
            });
        } else if let Some(block) = current.as_mut() {
            // `ProxyCommand` and `ProxyJump` are one setting to ssh: the first
            // of them wins (`ssh -G`: a `ProxyJump` after a `ProxyCommand`
            // is dropped, and the other way round). The command is kept
            // whole, to tell `none` from a command.
            if PROXY_KEYS.contains(&key.as_str())
                && PROXY_KEYS.iter().any(|k| block.kv.contains_key(*k))
            {
                continue;
            }
            let value = match key.as_str() {
                "proxycommand" => words.join(" "),
                _ => words.into_iter().next().unwrap_or_default(),
            };
            // First occurrence wins, matching ssh's own precedence.
            block.kv.entry(key).or_insert(value);
        }
    }

    if let Some(block) = current.take() {
        push(&mut blocks, block);
    }

    (blocks, error)
}

/// Whether `name` matches one ssh pattern: `*` stands for any run of
/// characters, `?` for one, anything else for itself, case included
/// (`ssh -G`: `Host A` applies to `ssh A`, not to `ssh a`).
fn ssh_pattern(pattern: &str, name: &str) -> bool {
    if !pattern.contains(['*', '?']) {
        return pattern == name;
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// Whether a `Host` line applies to `alias`, as ssh decides it: one of its
/// patterns matches and none of its `!` patterns does (`ssh -G`: `Host * !b`
/// skips `b`, and `Host !c` alone applies to no host at all).
fn host_matches(patterns: &[String], alias: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        match pattern.strip_prefix('!') {
            Some(negated) if ssh_pattern(negated, alias) => return false,
            Some(_) => {}
            None => matched |= ssh_pattern(pattern, alias),
        }
    }
    matched
}

/// The blocks of a file, with the `Host` blocks whose patterns each name one
/// host indexed by that name, so the values of a host are read from its own
/// blocks and the others (global, wildcard, `!`, `Match`) alone, not by
/// matching all of a 10,000-host file for each of its hosts.
struct SshConfig {
    blocks: Vec<HostBlock>,
    by_name: HashMap<String, Vec<usize>>,
    others: Vec<usize>,
    /// Whether a `Match` line names `final`, so ssh reads the file twice.
    second_pass: bool,
}

impl SshConfig {
    fn new(blocks: Vec<HostBlock>) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut others = Vec::new();
        for (index, block) in blocks.iter().enumerate() {
            let literal = block.scope == Scope::Host
                && block
                    .aliases
                    .iter()
                    .all(|a| !is_wildcard_pattern(a) && !a.starts_with('!'));
            if !literal {
                others.push(index);
                continue;
            }
            // A name twice on one line visits the block twice, which
            // changes nothing: the first value of each key is kept.
            for alias in &block.aliases {
                by_name.entry(alias.clone()).or_default().push(index);
            }
        }
        let second_pass = blocks.iter().any(|block| match &block.scope {
            Scope::Match(line) => line.criteria.contains(&(false, Criterion::Final)),
            _ => false,
        });
        Self {
            blocks,
            by_name,
            others,
            second_pass,
        }
    }

    /// The blocks that may apply to `alias`, in file order.
    fn candidates(&self, alias: &str) -> Vec<&HostBlock> {
        let mut indexes: Vec<usize> = self.others.clone();
        indexes.extend(self.by_name.get(alias).into_iter().flatten());
        indexes.sort_unstable();
        indexes.into_iter().map(|i| &self.blocks[i]).collect()
    }
}

/// The keys of a host's profile, as ssh reads them for it.
const PROFILE_KEYS: [&str; 6] = [
    "hostname",
    "port",
    "user",
    "identityfile",
    "proxyjump",
    "proxycommand",
];

/// The two keys ssh reads as one setting: the first of them obtained wins.
const PROXY_KEYS: [&str; 2] = ["proxyjump", "proxycommand"];

/// The values ssh reads for the host `alias`: over every block that applies
/// to it, in file order, the first value of each key (`ssh -G` on 10.2p1: a
/// `User` before the first `Host`, a later `Host *` and a `Match all` all
/// set what the host's own block leaves unset, and never what it sets). For
/// `IdentityFile`, which ssh gathers from every block, the first is the key
/// ssh tries first, the one the profile keeps.
///
/// A `Match` is read from the file ([`match_holds`]). One whose conditions
/// are known only when ssh runs (`exec`, `localnetwork` ...) is not
/// evaluated: when it sets a key the host has no value for yet, the value
/// ssh uses is unknown, and `Err` gives the reason the host is skipped.
///
/// ssh reads the file a second time when a `Match` line names `final`, or
/// `CanonicalizeHostname` is `yes` or `always` for the host: `Host` patterns
/// then match the host name read so far, lowercase, and `canonical` and
/// `final` hold (`ssh -G`: `Host a` / `HostName real.example.com` with a
/// `Host real.example.com` / `Port 2345` and an empty `Match final` gives
/// port 2345). With `CanonicalDomains` too, ssh may rename the host through
/// DNS before that reading, which the import cannot know: the host is
/// skipped. The file's `Include` lines are not read.
fn host_values(config: &SshConfig, alias: &str) -> Result<HashMap<String, String>, String> {
    let mut values: HashMap<String, String> = HashMap::new();
    read_pass(config, &mut values, alias, alias, false)?;
    let canonicalize = values
        .get("canonicalizehostname")
        .is_some_and(|v| v.eq_ignore_ascii_case("yes") || v.eq_ignore_ascii_case("always"));
    if canonicalize && values.contains_key("canonicaldomains") {
        return Err("ssh may rename its host through DNS (`CanonicalizeHostname` with `CanonicalDomains`) and read the file again for the new name, which this import cannot know; set the full host name in `HostName` and turn `CanonicalizeHostname` off for it to import it".to_string());
    }
    if canonicalize || config.second_pass {
        let host = match_host(&values, alias);
        read_pass(config, &mut values, &host, alias, true)?;
    }
    Ok(values)
}

/// One reading of the file for `alias`, with `Host` patterns matched against
/// `name` (the alias, then the host name in the second reading).
fn read_pass(
    config: &SshConfig,
    values: &mut HashMap<String, String>,
    name: &str,
    alias: &str,
    second: bool,
) -> Result<(), String> {
    for block in config.candidates(name) {
        let applies = match &block.scope {
            Scope::Global => true,
            Scope::Host => host_matches(&block.aliases, name),
            Scope::Match(line) => match match_holds(line, values, alias, second) {
                Some(holds) => holds,
                None => {
                    let unset = PROFILE_KEYS.iter().find(|key| {
                        block.kv.contains_key(**key)
                            && !values.contains_key(**key)
                            && !(PROXY_KEYS.contains(*key)
                                && PROXY_KEYS.iter().any(|k| values.contains_key(*k)))
                    });
                    if let Some(key) = unset {
                        return Err(format!(
                            "the `Match` block on line {} of the file may set its `{key}`, on a condition known only when ssh runs; give the host its `{key}` in its own `Host` block and import again",
                            line.line
                        ));
                    }
                    false
                }
            },
        };
        if applies {
            for (key, value) in &block.kv {
                if PROXY_KEYS.contains(&key.as_str())
                    && PROXY_KEYS.iter().any(|k| values.contains_key(*k))
                {
                    continue;
                }
                values.entry(key.clone()).or_insert_with(|| value.clone());
            }
        }
    }
    Ok(())
}

/// The host name a `Match host` compares, and the second reading matches
/// `Host` patterns against: the `HostName` read so far with its `%h`
/// expanded, or else the alias, lowercase as ssh makes it.
fn match_host(values: &HashMap<String, String>, alias: &str) -> String {
    let tokens = SshTokens {
        alias,
        host: alias,
        user: None,
        port: 22,
    };
    values
        .get("hostname")
        .filter(|h| !h.is_empty())
        .and_then(|h| ssh_expand("host", h, "h", &tokens, false).ok())
        .unwrap_or_else(|| alias.to_string())
        .to_lowercase()
}

/// Whether a `Match` line holds for `alias` with the values read so far:
/// `Some` when every condition it needs is known from the file, `None` when
/// one is known only when ssh runs and none of the others is false.
fn match_holds(
    line: &MatchLine,
    values: &HashMap<String, String>,
    alias: &str,
    second: bool,
) -> Option<bool> {
    let mut known = true;
    for (negated, criterion) in &line.criteria {
        let holds = match criterion {
            Criterion::All => Some(true),
            Criterion::Canonical | Criterion::Final => Some(second),
            Criterion::Host(patterns) => Some(pattern_list_matches(
                patterns,
                &match_host(values, alias),
                true,
            )),
            Criterion::OriginalHost(patterns) => Some(pattern_list_matches(patterns, alias, true)),
            Criterion::User(patterns) => values
                .get("user")
                .cloned()
                .or_else(|| local_account().0)
                .map(|user| pattern_list_matches(patterns, &user, false)),
            Criterion::LocalUser(patterns) => local_account()
                .0
                .map(|user| pattern_list_matches(patterns, &user, false)),
            Criterion::Runtime => None,
        };
        match holds.map(|h| h != *negated) {
            Some(false) => return Some(false),
            Some(true) => {}
            None => known = false,
        }
    }
    known.then_some(true)
}

/// Whether `name` matches a comma-separated ssh pattern list: one of its
/// patterns does and none of its `!` patterns does (`ssh -G`: `Match host
/// a,!a` and `!a,a` hold for no host). `any_case` compares lowercase, as
/// `Match host` and `originalhost` do.
fn pattern_list_matches(list: &str, name: &str, any_case: bool) -> bool {
    let (list, name) = if any_case {
        (list.to_lowercase(), name.to_lowercase())
    } else {
        (list.to_string(), name.to_string())
    };
    let patterns: Vec<String> = list.split(',').map(str::to_string).collect();
    host_matches(&patterns, &name)
}

/// The host a `Host` block is imported as: its first pattern that names one
/// host, neither a wildcard nor a `!` exclusion.
fn profile_alias(block: &HostBlock) -> Result<String, String> {
    if block.aliases.iter().any(|a| is_wildcard_pattern(a)) {
        return Err("wildcard host pattern".to_string());
    }
    match block.aliases.iter().find(|a| !a.is_empty()) {
        None => Err("empty host alias".to_string()),
        Some(_) => block
            .aliases
            .iter()
            .find(|a| !a.starts_with('!'))
            .cloned()
            .ok_or_else(|| {
                "its `Host` line only excludes hosts, and ssh applies such a line to no host"
                    .to_string()
            }),
    }
}

/// Convert the `Host` block at `index` of the file to a profile, with the
/// values ssh reads for its host from every block ([`host_values`]), or the
/// reason it is skipped: a wildcard pattern, no usable alias, or a value
/// that cannot be imported.
fn map_host_block(config: &SshConfig, index: usize) -> Result<ServerProfileExport, String> {
    let alias = profile_alias(&config.blocks[index])?;
    let kv = &host_values(config, &alias)?;
    // A port in digits outside 1 to 65535 already refused the whole file.
    let port = match kv.get("port") {
        Some(p) => match port_digits(p) {
            Some(digits) => digits
                .parse::<u16>()
                .map(u32::from)
                .map_err(|e| e.to_string())?,
            None => service_port(p)?,
        },
        None => 22,
    };
    // ssh expands `HostName` first (its `%h` is the alias), then the others
    // against that host name: the order the tokens are resolved in below.
    let mut tokens = SshTokens {
        alias: &alias,
        host: &alias,
        user: None,
        port,
    };
    // ssh lowercases the host name, `%h` included (`ssh -vvv A` with
    // `IdentityFile /k/%h_%n` loads `/k/a_A`).
    let host = match kv.get("hostname").filter(|h| !h.is_empty()) {
        Some(h) => ssh_expand("host", h, "h", &tokens, false)?,
        None => alias.clone(),
    }
    .to_lowercase();
    tokens.host = &host;
    let username = match kv.get("user") {
        Some(u) => ssh_expand("user name", u, "dhnpu", &tokens, true)?,
        None => String::new(),
    };
    // With no `User`, ssh connects as the local account and expands `%r`
    // to its name (ssh.c sets the user before it expands `IdentityFile`);
    // when that name cannot be read, `%r` skips the host, never an empty
    // name. The profile keeps no user name, as before.
    let local_user = local_account().0;
    tokens.user = if kv.contains_key("user") {
        Some(&username)
    } else {
        local_user.as_deref()
    };

    let mut options = serde_json::Map::new();
    if let Some(identity) = kv.get("identityfile").filter(|s| !s.is_empty()) {
        let identity = ssh_expand("key file path", identity, "dhnpru", &tokens, true)?;
        options.insert(
            "private_key_path".into(),
            serde_json::Value::String(shellexpand_tilde(&identity)?),
        );
    }
    // `none` turns either off (`ssh -G`: `ProxyJump none`, any case, is no
    // jump; a `ProxyCommand none` first leaves out a later `ProxyJump`).
    if let Some(command) = kv
        .get("proxycommand")
        .filter(|c| !c.eq_ignore_ascii_case("none"))
    {
        let _ = command;
        return Err("ssh reaches it through a `ProxyCommand`, which AeroFTP does not run; use `ProxyJump` for a jump host and import again".to_string());
    }
    if let Some(proxy) = kv
        .get("proxyjump")
        .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))
    {
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

/// A `Host` block that was skipped, with the reason: a wildcard or `!`-only
/// pattern, no usable alias, an earlier block of the same host, a line ssh
/// refuses, a `Match` known only when ssh runs that may set one of its
/// values, a host name DNS may change, a `ProxyCommand`, or a value that
/// cannot be imported.
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
    let config = SshConfig::new(blocks);
    let total_remotes = config
        .blocks
        .iter()
        .filter(|b| b.scope == Scope::Host)
        .count();
    let mut servers = Vec::new();
    let mut skipped = Vec::new();
    let mut seen = std::collections::HashSet::new();

    for (index, block) in config.blocks.iter().enumerate() {
        if block.scope != Scope::Host {
            continue;
        }
        let display_name = block.aliases.first().cloned().unwrap_or_default();
        // ssh reads every block that names a host as one, so the host's
        // profile comes from the first of them and holds the others' values.
        let mapped = match (&error, profile_alias(block)) {
            (Some(reason), _) => Err(reason.clone()),
            (None, Ok(alias)) if !seen.insert(alias.clone()) => Err(
                "an earlier `Host` block names the same host, and ssh reads the two as one: it is imported once, from the first"
                    .to_string(),
            ),
            (None, _) => map_host_block(&config, index),
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
        let profile = map_host_block(&SshConfig::new(blocks), 0).expect("should map");
        assert_eq!(profile.host, "fallbackhost");
        assert_eq!(profile.username, "bob");
    }

    #[test]
    fn test_secret_policy_never_credential() {
        let blocks = parse_ssh_config(
            "Host keyed\n  HostName h.example.com\n  IdentityFile ~/.ssh/id_rsa\n",
        )
        .0;
        let profile = map_host_block(&SshConfig::new(blocks), 0).expect("should map");
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

    /// ssh reads `~/.ssh/config` from the account's home, not from `HOME`,
    /// and so does the detection: a config under another `HOME` is not it.
    #[test]
    fn test_default_path_is_the_accounts_not_home() {
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
            let in_dir = Some(dir.join(".ssh").join("config"));
            assert_eq!(ssh_config_in(Some(dir.display().to_string())), in_dir);
            assert_eq!(
                ssh_config_in(Some(dir.join("none").display().to_string())),
                None
            );

            std::env::set_var("HOME", &dir);
            let detected = default_ssh_config_path();

            match prev {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            std::fs::remove_dir_all(&dir).ok();
            assert_ne!(detected, in_dir);
            assert_eq!(detected, ssh_config_in(local_account().1));
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

    /// A host takes from the other blocks that apply to it what its own
    /// leaves unset, as `ssh -G` reads the same file: the lines before the
    /// first `Host`, `Host` patterns with `*`, `?` and `!`, a second block of
    /// the same host, `Match all`. A `Match` on another condition that may
    /// set a value the host has none for yet skips the host.
    #[test]
    fn a_host_takes_what_other_blocks_set_as_ssh_reads_it() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "IdentityFile /k/global\n\
             Host * !b\n  User star\n\
             Host a? z\n  Port 2202\n\
             Host a\n  HostName a.example.com\n\
             Host a\n  User second\n  IdentityFile /k/second\n\
             Host !c\n  Port 2299\n\
             Host b\n  HostName b.example.com\n\
             Host ab\n\
             Host z\n  User zown\n\
             Host c\n\
             Match all\n  HostName %h.m.example.com\n  ProxyJump pj\n\
             Host b\n  Port 2400\n",
        )
        .unwrap();
        let result = import_ssh_config(&config).unwrap();
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(result.total_remotes, 10);
        assert_eq!(
            skipped,
            [
                ("*", "wildcard host pattern"),
                ("a?", "wildcard host pattern"),
                ("a", "an earlier `Host` block names the same host, and ssh reads the two as one: it is imported once, from the first"),
                ("!c", "its `Host` line only excludes hosts, and ssh applies such a line to no host"),
                ("b", "an earlier `Host` block names the same host, and ssh reads the two as one: it is imported once, from the first"),
            ]
        );
        let got: Vec<(&str, &str, u32, &str, &str, &str)> = result
            .servers
            .iter()
            .map(|s| {
                let o = s.options.as_ref().unwrap();
                (
                    s.name.as_str(),
                    s.host.as_str(),
                    s.port,
                    s.username.as_str(),
                    o["private_key_path"].as_str().unwrap(),
                    o.get("proxyJump").and_then(|v| v.as_str()).unwrap_or(""),
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("a", "a.example.com", 22, "star", "/k/global", "pj"),
                ("b", "b.example.com", 2400, "", "/k/global", "pj"),
                ("ab", "ab.m.example.com", 2202, "star", "/k/global", "pj"),
                ("z", "z.m.example.com", 2202, "star", "/k/global", "pj"),
                ("c", "c.m.example.com", 22, "star", "/k/global", "pj"),
            ]
        );

        // The two cases of the review: a `Match all` after the host gives it
        // the `HostName` and `Port` it does not set, and never one it sets.
        let match_all = dir.path().join("match_all");
        std::fs::write(
            &match_all,
            "Host lab\n  HostName lab.example.com\n\
             Host storage\n\
             Match all\n  HostName storage.example.com\n  Port 2222\n",
        )
        .unwrap();
        let result = import_ssh_config(&match_all).unwrap();
        let hosts: Vec<(&str, &str, u32)> = result
            .servers
            .iter()
            .map(|s| (s.name.as_str(), s.host.as_str(), s.port))
            .collect();
        assert_eq!(
            hosts,
            [
                ("lab", "lab.example.com", 2222),
                ("storage", "storage.example.com", 2222)
            ]
        );

        // `ProxyCommand` and `ProxyJump` are one setting, the first wins; a
        // command skips its host. `CanonicalizeHostname` reads the file a
        // second time; with `CanonicalDomains`, DNS may rename the host.
        let proxy = dir.path().join("proxy");
        std::fs::write(
            &proxy,
            "Host pc\n  ProxyCommand nc %h %p\n  ProxyJump j\n\
             Host pj\n  ProxyJump j\n  ProxyCommand nc %h %p\n\
             Host cy\n  CanonicalizeHostname yes\n  HostName real2.example.com\n\
             Host real2.example.com\n  User r2\n\
             Host cn\n  CanonicalizeHostname yes\n  CanonicalDomains example.com\n",
        )
        .unwrap();
        let result = import_ssh_config(&proxy).unwrap();
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                ("pc", "ssh reaches it through a `ProxyCommand`, which AeroFTP does not run; use `ProxyJump` for a jump host and import again"),
                ("cn", "ssh may rename its host through DNS (`CanonicalizeHostname` with `CanonicalDomains`) and read the file again for the new name, which this import cannot know; set the full host name in `HostName` and turn `CanonicalizeHostname` off for it to import it"),
            ]
        );
        let proxy_got: Vec<(&str, &str, u32, &str, &str, &str)> = result
            .servers
            .iter()
            .map(|s| {
                let jump = s
                    .options
                    .as_ref()
                    .and_then(|o| o.get("proxyJump"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                (
                    s.name.as_str(),
                    s.host.as_str(),
                    s.port,
                    s.username.as_str(),
                    "",
                    jump,
                )
            })
            .collect();
        let proxy_want = [
            ("pj", "pj", 22, "", "", "j"),
            ("cy", "real2.example.com", 22, "r2", "", ""),
            ("real2.example.com", "real2.example.com", 22, "r2", "", ""),
        ];
        assert_eq!(proxy_got, proxy_want);

        // `Match` conditions the file decides, and the second reading a
        // `Match final` asks for; `ProxyCommand none` and `ProxyJump none`.
        let decided = dir.path().join("decided");
        std::fs::write(
            &decided,
            "Host web\n  HostName web.example.com\n  User deploy\n\
             Match host *.internal\n  ProxyJump bastion\n\
             Host gitlab\n  HostName gitlab.example.com\n\
             Match originalhost gitlab\n  User git\n\
             Match host web.example.com\n  Port 2201\n\
             Match user deploy\n  IdentityFile /k/deploy\n\
             Match localuser * !originalhost web\n  Port 2202\n\
             Host real\n  HostName Real.Example.com\n\
             Host real.example.com\n  User r\n\
             Host plain\n  ProxyCommand none\n\
             Host none\n  ProxyJump NONE\n\
             Host up\n  HostName Up.Example.com\n\
             Match host UP.example.COM\n  User upcase\n\
             Host *\n  ProxyJump j2\n\
             Match final\n",
        )
        .unwrap();
        let result = import_ssh_config(&decided).unwrap();
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(skipped, [("*", "wildcard host pattern")]);
        let decided_got: Vec<(&str, &str, u32, &str, &str, &str)> = result
            .servers
            .iter()
            .map(|s| {
                let o = s.options.as_ref();
                let opt = |k: &str| {
                    o.and_then(|o| o.get(k))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                };
                (
                    s.name.as_str(),
                    s.host.as_str(),
                    s.port,
                    s.username.as_str(),
                    opt("private_key_path"),
                    opt("proxyJump"),
                )
            })
            .collect();
        let decided_want = [
            ("web", "web.example.com", 2201, "deploy", "/k/deploy", "j2"),
            ("gitlab", "gitlab.example.com", 2202, "git", "", "j2"),
            ("real", "real.example.com", 2202, "r", "", "j2"),
            ("real.example.com", "real.example.com", 2202, "r", "", "j2"),
            ("plain", "plain", 2202, "", "", ""),
            ("none", "none", 2202, "", "", ""),
            ("up", "up.example.com", 2202, "upcase", "", "j2"),
        ];
        assert_eq!(decided_got, decided_want);

        // A condition known only when ssh runs: a host is skipped when such
        // a `Match` may set a value it has none for yet, and only then.
        let unevaluated = dir.path().join("unevaluated");
        std::fs::write(
            &unevaluated,
            "Host e\n  HostName e.example.com\n  User eown\n  Port 2202\n\
             Host f\n  HostName f.example.com\n  Port 2204\n\
             Match host f.example.com exec \"false\"\n  User mf\n\
             Match host nomatch exec \"x\"\n  User never\n\
             Match exec \"true\"\n  Port 2500\n\
             Host g\n  User gu\n",
        )
        .unwrap();
        let result = import_ssh_config(&unevaluated).unwrap();
        let names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["e"]);
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        let reason = |line: usize, key: &str| {
            format!("the `Match` block on line {line} of the file may set its `{key}`, on a condition known only when ssh runs; give the host its `{key}` in its own `Host` block and import again")
        };
        assert_eq!(
            skipped,
            [
                ("f", reason(8, "user").as_str()),
                ("g", reason(12, "port").as_str())
            ]
        );

        if std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .is_err()
        {
            eprintln!(
                "SKIPPED: no ssh on this machine, so the values are not compared with ssh -G"
            );
            return;
        }
        let lab = [
            ("lab", "lab.example.com", 2222, "", "", ""),
            ("storage", "storage.example.com", 2222, "", "", ""),
        ];
        let got = got.into_iter().map(|g| (&config, g));
        let decided_got = decided_want.map(|g| (&decided, g));
        for (file, (name, host, port, user, key, jump)) in got
            .chain(lab.map(|g| (&match_all, g)))
            .chain(decided_got)
            .chain(proxy_want.map(|g| (&proxy, g)))
        {
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(file)
                .arg(name)
                .output()
                .unwrap();
            let read = String::from_utf8_lossy(&out.stdout).to_string();
            let first = |key: &str| {
                read.lines()
                    .find_map(|l| l.strip_prefix(&format!("{key} ")))
                    .unwrap_or("")
                    .to_string()
            };
            assert_eq!(first("hostname"), host, "{name}: {read}");
            assert_eq!(first("port"), port.to_string(), "{name}: {read}");
            if !user.is_empty() {
                assert_eq!(first("user"), user, "{name}: {read}");
            }
            if file != &match_all {
                if !key.is_empty() {
                    assert_eq!(first("identityfile"), key, "{name}: {read}");
                }
                assert_eq!(first("proxyjump"), jump, "{name}: {read}");
            }
        }
    }

    /// `%u`, `%d` and a `%r` with no `User` are the local account's, read
    /// from its passwd entry as ssh does, whatever `USER` and `HOME` say;
    /// so is the folder a leading `~/` stands for.
    #[cfg(unix)]
    #[test]
    fn the_local_tokens_are_the_accounts_not_the_environment() {
        let _env = crate::test_env::lock();
        // The account as the system reports it, not as `local_account` reads
        // it: `id -un`, and the home a shell gives `~name`.
        let run = |cmd: &str| {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .output()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let name = run("id -un");
        let dir = run(&format!("eval echo ~{name}"));
        assert!(!name.is_empty() && dir.starts_with('/'), "{name} {dir}");
        assert_eq!(local_account(), (Some(name.clone()), Some(dir.clone())));
        let saved: Vec<_> = ["HOME", "USER"]
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        std::env::set_var("HOME", "/tmp/aeroftp-not-the-home");
        std::env::set_var("USER", "not-the-user");
        let dir_path = tempfile::tempdir().unwrap();
        let source = dir_path.path().join("config");
        std::fs::write(
            &source,
            "Host a\n  HostName h.example.com\n  IdentityFile %d/id_%u_%r\n\nHost t\n  HostName h.example.com\n  IdentityFile ~/k\n",
        )
        .unwrap();
        let result = import_ssh_config(&source);
        for (key, value) in saved {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
        let result = result.unwrap();
        assert_eq!(result.servers.len(), 2, "{:?}", result.skipped.len());
        let a = &result.servers[0];
        assert_eq!(a.username, "");
        assert_eq!(
            a.options.as_ref().unwrap()["private_key_path"],
            format!("{dir}/id_{name}_{name}")
        );
        assert_eq!(
            result.servers[1].options.as_ref().unwrap()["private_key_path"],
            format!("{dir}/k")
        );
        // A user name that cannot be read is refused, not taken as empty.
        let unknown = SshTokens {
            alias: "a",
            host: "a",
            user: None,
            port: 22,
        };
        assert_eq!(
            ssh_expand("key file path", "/k/id_%r", "dhnpru", &unknown, true),
            Err(
                "its key file path uses the ssh token %r, which AeroFTP cannot resolve".to_string()
            )
        );
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
        let home = local_account().1.unwrap();
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
            eprintln!(
                "SKIPPED: no ssh on this machine, so the values are not compared with ssh -G"
            );
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
    /// and a `Match all` block gives the host before it what it leaves unset.
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
        assert_eq!(by("m").username, "matched");
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
            (
                "Host fine\n  HostName f.example.com\nMatch\n  Port 1\n",
                3,
                "gives `Match` no condition",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch bogus x\n  Port 1\n",
                3,
                "gives `Match` the condition `bogus`, which ssh does not know",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch host\n  Port 1\n",
                3,
                "gives the `Match` condition `host` no value",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch host=\n  Port 1\n",
                3,
                "gives the `Match` condition `host` no value",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch all host x\n  Port 1\n",
                3,
                "combines `Match all` with another condition",
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
        // A service name is looked up as ssh does it: `Port ssh` is 22, in
        // a host's block or before the first `Host`; a name the services
        // file does not list (`22x`, `Ssh`) skips only its host.
        #[cfg(unix)]
        for config in [
            "Host fine\n  HostName f.example.com\nHost svc\n  Port ssh\n",
            "Port ssh\nHost fine\n  HostName f.example.com\nHost svc\n",
        ] {
            std::fs::write(&bad, config).unwrap();
            let result = import_ssh_config(&bad).unwrap();
            let ports: Vec<(&str, u32)> = result
                .servers
                .iter()
                .map(|s| (s.name.as_str(), s.port))
                .collect();
            assert_eq!(ports, [("fine", 22), ("svc", 22)], "{config:?}");
        }
        for name in ["22x", "Ssh"] {
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
                result.skipped[0].reason.starts_with(&format!(
                    "its port `{name}` is a service name the services file does not list"
                )),
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
            eprintln!(
                "SKIPPED: no ssh on this machine, so the values are not compared with ssh -G"
            );
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
