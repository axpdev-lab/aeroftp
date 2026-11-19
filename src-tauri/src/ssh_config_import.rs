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

/// Expand a leading `~` of a key path as ssh does, before its tokens
/// (`tilde_expand` in misc.c, measured with `ssh -vvv` on 10.2p1): the name
/// up to the first `/` is an account, its home folder from the passwd entry
/// (`lookup`), or, with no name, the local account's (`home`: `HOME=/tmp`
/// still loads `~/k` from the passwd home; only `${HOME}` reads the
/// environment); the rest follows after one `/` (`~root//k` is `/root/k`,
/// `~root` and `~/` are the home folder with a `/`). ssh stops when the
/// account does not exist (`~nosuch/k`: "No such user"), and the host is
/// skipped. Any other path is returned unchanged.
fn ssh_tilde(
    p: &str,
    home: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let Some(after) = p.strip_prefix('~') else {
        return Ok(p.to_string());
    };
    let (user, rest) = after.split_once('/').unwrap_or((after, ""));
    let dir = if user.is_empty() {
        home.map(str::to_string).ok_or_else(|| {
            "its key file path starts with `~`, and the local account's home folder could not be read".to_string()
        })?
    } else {
        // The name is the file's: it is not repeated.
        lookup(user).ok_or_else(|| {
            "its key file path starts with `~` and the name of an account this system does not have, and ssh stops there".to_string()
        })?
    };
    let slash = if dir.ends_with('/') { "" } else { "/" };
    Ok(format!("{dir}{slash}{}", rest.trim_start_matches('/')))
}

/// The home folder of the account `user` from its passwd entry, as ssh
/// reads it for `~user` (`getpwnam`); elsewhere `None`, so such a key path
/// skips its host instead of being kept as a path nothing reads.
fn passwd_home(user: &str) -> Option<String> {
    #[cfg(unix)]
    {
        let name = std::ffi::CString::new(user).ok()?;
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut buf = vec![0 as libc::c_char; 16 * 1024];
        let mut result: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: the name ends in NUL, the buffers outlive the call, and
        // the string read below points into `buf`, alive while it is copied.
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                &mut pwd,
                buf.as_mut_ptr(),
                buf.len(),
                &mut result,
            )
        };
        if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        // SAFETY: a non-null `pw_dir` points to a NUL-ended string in `buf`.
        unsafe { std::ffi::CStr::from_ptr(pwd.pw_dir) }
            .to_str()
            .ok()
            .map(str::to_string)
    }
    #[cfg(not(unix))]
    {
        let _ = user;
        None
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
    /// `%u` and `%d`: the local account, read once for the import.
    account: &'a Account,
    /// The import's work, which every byte expanded adds to.
    steps: &'a Steps,
}

/// The longest value an expansion gives, in bytes: longer than any host
/// name (255), user name or key path (4,096 on Linux) a client takes, and
/// short enough that one value, a long alias in every `%h` of a `HostName`
/// among them, cannot hold the memory of the import.
const MAX_EXPANDED: usize = 64 * 1024;

/// The longest variable name a skip reason repeats: far longer than any
/// environment variable's, and short enough that a reason kept for each of
/// thousands of hosts stays small.
const MAX_REPORTED_NAME: usize = 64;

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
    // Checked before each piece is added, so the value never grows past it.
    let too_long = || {
        format!("its {field} is longer than {MAX_EXPANDED} bytes once its tokens are expanded, longer than any host name, user name or key path; write it out and import again")
    };
    let add = |out: &mut String, piece: &str| {
        tokens.steps.charge(piece.len() as u64);
        if out.len() + piece.len() > MAX_EXPANDED {
            return Err(too_long());
        }
        out.push_str(piece);
        Ok(())
    };
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && dollar && chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            // Each byte read is work the file decides, repeated for every host
            // a global line applies to.
            let closed = loop {
                match chars.next() {
                    Some('}') => break true,
                    Some(c) => {
                        tokens.steps.charge(c.len_utf8() as u64);
                        name.push(c)
                    }
                    None => break false,
                }
            };
            if !closed {
                return Err(format!("its {field} holds a `${{` that is never closed"));
            }
            if !SSH_IMPORT_ENV.contains(&name.as_str()) {
                // The reason is kept for every host the line applies to, so a
                // name longer than any variable's is not repeated in it.
                let named = match name.len() <= MAX_REPORTED_NAME {
                    true => format!("the environment variable `{name}`"),
                    false => format!("an environment variable whose name is longer than {MAX_REPORTED_NAME} bytes"),
                };
                return Err(format!(
                    "its {field} reads {named}, which an import does not read; only {} are expanded",
                    SSH_IMPORT_ENV.join(", ")
                ));
            }
            let found = env(&name).ok_or_else(|| {
                format!("its {field} reads the environment variable `{name}`, which is not set")
            })?;
            add(&mut out, &found)?;
            continue;
        }
        if c != '%' {
            add(&mut out, c.encode_utf8(&mut [0; 4]))?;
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
                'u' => tokens.account.0.clone(),
                'd' => tokens.account.1.clone(),
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
        add(&mut out, &expanded)?;
    }
    Ok(out)
}

/// The local account's name and home folder, as ssh reads them for `%u`, `%d`
/// and a `%r` with no `User`: on Unix the passwd entry of the real user, as ssh reads it (`getuid`),
/// not `USER` or `HOME` (`ssh -G` with `USER=foo` still gives the account's
/// name); elsewhere the environment, as Win32-OpenSSH reads it. An import
/// reads it once ([`SshConfig::account`]), not once per host and rule.
fn local_account() -> Account {
    #[cfg(test)]
    tests::ACCOUNT_READS.with(|n| n.set(n.get() + 1));
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

/// The local account's name and home folder ([`local_account`]).
type Account = (Option<String>, Option<String>);

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
/// "Bad Match condition"; so is an empty value, except for `tagged` and
/// `command`, and a value given to `all`, `canonical` or `final`). A
/// condition may be written `name=value`.
fn parse_match(words: &[String]) -> Result<Vec<(bool, Criterion)>, String> {
    if words.is_empty() {
        return Err("gives `Match` no condition".to_string());
    }
    let mut criteria = Vec::new();
    let mut rest = words.iter();
    while let Some(word) = rest.next() {
        // A word that starts with `#`, quoted or not, ends the conditions
        // (`ssh -G`: `Match host x "#y" z` reads as `Match host x`).
        if word.starts_with('#') {
            break;
        }
        let (negated, word) = match word.strip_prefix('!') {
            Some(word) => (true, word),
            None => (false, word.as_str()),
        };
        let (name, inline) = match word.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (word, None),
        };
        let name = name.to_ascii_lowercase();
        // `all`, `canonical` and `final` take no value: `Match final=x` is
        // "missing argument" to ssh.
        if matches!(name.as_str(), "all" | "canonical" | "final") && inline.is_some() {
            return Err(format!("gives the `Match` condition `{name}` a value"));
        }
        let criterion = match name.as_str() {
            // `all` may follow one condition and be followed by none, but
            // an empty word or a comment (`ssh -G` on 10.2p1: `Match host x
            // all` and `Match all ""` are read, `Match final all host x`,
            // `Match final host x all` and `Match all "" x` refused).
            "all" => {
                let alone = match rest.as_slice() {
                    [] => true,
                    [next, ..] if next.starts_with('#') => true,
                    [next] => next.is_empty(),
                    _ => false,
                };
                if criteria.len() > 1 || !alone {
                    return Err("combines `Match all` with another condition".to_string());
                }
                criteria.push((negated, Criterion::All));
                break;
            }
            "canonical" => Criterion::Canonical,
            "final" => Criterion::Final,
            "host" | "originalhost" | "user" | "localuser" | "exec" | "localnetwork" | "tagged"
            | "version" | "sessiontype" | "command" => {
                // An empty value is refused too, except for `tagged` and
                // `command` (`ssh -G`: `Match tagged ""` applies), and so is
                // one that starts with `#`, quoted or not, for every
                // condition (`ssh -G`: `Match host "#lab"` and `Match tagged
                // "#x"` are "Missing Match criteria").
                let value = match inline {
                    Some(value) => Some(value.to_string()),
                    None => rest.next().cloned(),
                }
                .filter(|v| !v.starts_with('#'))
                .filter(|v| !v.is_empty() || matches!(name.as_str(), "tagged" | "command"))
                .ok_or_else(|| format!("gives the `Match` condition `{name}` no value"))?;
                match name.as_str() {
                    "host" => Criterion::Host(value),
                    "originalhost" => Criterion::OriginalHost(value),
                    "user" => Criterion::User(value),
                    "localuser" => Criterion::LocalUser(value),
                    _ => Criterion::Runtime,
                }
            }
            // The word is not repeated: the file is not trusted, and that
            // position may hold anything.
            _ => return Err("gives `Match` a condition ssh does not know".to_string()),
        };
        criteria.push((negated, criterion));
    }
    if criteria.is_empty() {
        return Err("gives `Match` no condition".to_string());
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
    /// Every `IdentityFile` line of the block, `none` included, in order:
    /// ssh counts them ([`MAX_IDENTITY_FILES`]). Each is the number of its
    /// text in the file, the same for the same text: a host compares up to
    /// 100 of them for each line, at the same cost whatever their length.
    identities: Vec<u32>,
    /// The line of the block's first `Include`, which the import does not
    /// read ([`read_pass`]).
    include: Option<usize>,
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
    if key == "proxycommand" {
        // The command is the rest of the line as written, quotes and `#`
        // included (`ssh -G`: `ProxyCommand "none"` and `ProxyCommand none
        // # c` are commands, not `none`); a quote left open is refused.
        if let Err(reason) = ssh_words(rest) {
            return Some(Err(reason));
        }
        return Some(Ok((key, vec![rest.trim().to_string()])));
    }
    if key == "proxyjump" {
        // ssh splits the line into words first (`argv_split`), so a quote
        // left open is refused here too: `ProxyJump "j` gives "invalid
        // quotes", while a quote inside a comment (`ProxyJump j # "x`) does not.
        if let Err(reason) = ssh_words(rest) {
            return Some(Err(reason));
        }
        // ssh compares the line as written with `none`, and reads the hops
        // without the comment (`ssh -vvv`: `ProxyJump none # c` jumps
        // through a host called `none`): that line is kept whole, `#`
        // included, for [`map_host_block`] to skip.
        let jump = rest.split('#').next().unwrap_or("").trim();
        let jump = if jump.eq_ignore_ascii_case("none") {
            rest.trim()
        } else {
            jump
        };
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

/// The most `IdentityFile` lines ssh reads for one host (`ssh -G` on
/// 10.2p1: the 101st, `none` and repeats included, is "Too many identity
/// files specified (max 100)", and ssh stops).
const MAX_IDENTITY_FILES: usize = 100;

/// The values `CanonicalizeHostname` takes, in any case (`ssh -G` on
/// 10.2p1: `YES` is read, `maybe` is "unsupported option").
const CANONICALIZE_VALUES: [&str; 5] = ["yes", "no", "true", "false", "always"];

/// Why ssh refuses a whole file for this line, for the keywords this import
/// reads (`ssh -G` on 10.2p1): a quote left open ("invalid quotes"), a
/// second value ("garbage at end of line"), no value ("Missing argument"),
/// a port that is not from 1 to 65535, or a service name the system does
/// not know ("Bad port": `Port ssh` is 22, `Port Ssh` refused), a jump host
/// ssh cannot read ([`ssh_jump_valid`]: "Invalid ProxyJump"), and a
/// `CanonicalizeHostname`, `CanonicalDomains` or
/// `CanonicalizePermittedCNAMEs` value it does not take. ssh checks every
/// line, before the first `Host` and inside a block that applies to no host
/// too, and then reads none of the hosts. A keyword this import does not
/// read is left to ssh: which ones exist, and how many values each takes,
/// depends on its version. No reason repeats the value: the file is not
/// trusted, and a value may hold anything.
fn ssh_line_error(key: &str, words: &[String], services: &Services) -> Option<String> {
    if key == "match" {
        return parse_match(words).err();
    }
    let reads = IMPORTED_KEYS.contains(&key)
        || PROXY_KEYS.contains(&key)
        || RESOLUTION_KEYS.contains(&key)
        || key == "host";
    let first = words.first().map(String::as_str).unwrap_or("");
    let bad_port = "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows".to_string();
    if !reads {
        None
    } else if first.is_empty() && words.len() <= 1 {
        Some(format!("gives `{key}` no value"))
    } else if words.len() > 1 && (IMPORTED_KEYS.contains(&key) || key == "canonicalizehostname") {
        Some(format!("gives `{key}` more than one value"))
    } else if key == "port" {
        // A services file that cannot be read skips the host instead.
        matches!(ssh_a2port(first, services), Ok(None | Some(0))).then_some(bad_port)
    } else if key == "proxyjump" {
        // `none` as written is no jump; `none # c` is a host called `none`.
        let hops = first.split('#').next().unwrap_or("").trim();
        (!first.eq_ignore_ascii_case("none") && !ssh_jump_valid(hops, services))
            .then(|| "gives `proxyjump` a jump host ssh cannot read".to_string())
    } else if key == "canonicalizehostname" {
        (!CANONICALIZE_VALUES
            .iter()
            .any(|v| first.eq_ignore_ascii_case(v)))
        .then(|| "gives `canonicalizehostname` a value ssh does not take".to_string())
    } else if matches!(key, "canonicaldomains" | "canonicalizepermittedcnames")
        && words.len() > 1
        && words.iter().any(|w| w.eq_ignore_ascii_case("none"))
    {
        Some(format!("gives `{key}` `none` with other values"))
    } else if key == "canonicaldomains" {
        (!words.iter().all(|w| ssh_valid_domain(w)))
            .then(|| "gives `canonicaldomains` a domain name ssh refuses".to_string())
    } else if key == "canonicalizepermittedcnames" {
        // `none`, `*`, or `from:to` with something after the colon (`ssh
        // -G`: `:b` is read, `a:` and `abc` are "Invalid permitted CNAME").
        let valid = |w: &String| {
            w.eq_ignore_ascii_case("none")
                || w == "*"
                || w.find(':').is_some_and(|i| i + 1 < w.len())
        };
        (!words.iter().all(valid))
            .then(|| "gives `canonicalizepermittedcnames` a rule ssh cannot read".to_string())
    } else {
        None
    }
}

/// Whether ssh reads `name` as a domain (`valid_domain` in misc.c): it
/// starts with an ASCII letter, a digit or `_`, holds only those, `-` and
/// `.`, and no `..` (`ssh -G`: `-x`, `a..b` and `Ä.b` are refused).
fn ssh_valid_domain(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes
        .first()
        .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        && !name.contains("..")
}

/// Whether ssh reads a `ProxyJump` value as jump hosts (`parse_jump` in
/// readconf.c, `parse_uri` and `parse_user_host_port` in misc.c, measured
/// with `ssh -G` on 10.2p1): hops split at every `,`, none of them empty,
/// each `ssh://[user[;params]@]host[:port][/]` with a domain for host, or
/// `[user@]host[:port]` with a `[...]` host or one without `:` or `/`, and
/// a port from 1 to 65535 or a known service name when one is written
/// (`ssh -G`: `a:`, `a@b@c`, `[::1]:22` and `ssh://u@a:22/` are read;
/// `a:0`, `a:x`, `@a`, `a/b`, `::1`, `a,,b`, `ssh://[::1]` and `ssh://a/x`
/// are refused). The hops are checked in every block, applied or not.
fn ssh_jump_valid(value: &str, services: &Services) -> bool {
    value
        .split(',')
        .all(|hop| match hop.strip_prefix("ssh://") {
            Some(uri) => ssh_uri_valid(uri, services),
            None => {
                let host = match hop.rfind('@') {
                    Some(0) => return false,
                    Some(at) => &hop[at + 1..],
                    None => hop,
                };
                // A port is read when something follows the `:`.
                match ssh_host_delim(host) {
                    Some((host, delim, port)) => {
                        !host.is_empty()
                            && delim != Some('/')
                            && (port.is_empty() || ssh_port_valid(port, services))
                    }
                    None => false,
                }
            }
        })
}

/// The `ssh://` form of a jump host, after the scheme ([`ssh_jump_valid`]).
fn ssh_uri_valid(uri: &str, services: &Services) -> bool {
    let host = match uri.find('@') {
        Some(at) => {
            let user = &uri[..at];
            let user = user.split(';').next().unwrap_or("");
            if user.is_empty() || !ssh_url_decodes(user) {
                return false;
            }
            &uri[at + 1..]
        }
        None => uri,
    };
    let Some((host, delim, rest)) = ssh_host_delim(host) else {
        return false;
    };
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() || !ssh_valid_domain(bare) {
        return false;
    }
    // A path is refused, a `/` with nothing after it is not; after a `:`,
    // anything is read as a port up to the next `/` (`ssh://a:/` refused).
    match delim {
        Some(':') if !rest.is_empty() => {
            let (port, path) = rest.split_once('/').unwrap_or((rest, ""));
            ssh_port_valid(port, services) && path.is_empty()
        }
        _ => rest.is_empty(),
    }
}

/// A host and what follows it, as ssh splits them (`hpdelim2` in misc.c):
/// a `[...]` host, or the text up to the first `:` or `/`, then that
/// delimiter and the rest. `None` for a `[` never closed, or a `]` followed
/// by anything else.
fn ssh_host_delim(s: &str) -> Option<(&str, Option<char>, &str)> {
    let end = if s.starts_with('[') {
        s.find(']')? + 1
    } else {
        s.find([':', '/']).unwrap_or(s.len())
    };
    match s[end..].chars().next() {
        None => Some((s, None, "")),
        Some(delim @ (':' | '/')) => Some((&s[..end], Some(delim), &s[end + 1..])),
        Some(_) => None,
    }
}

/// Whether ssh reads a written port of a jump host as one: [`ssh_a2port`]
/// gives a port above 0. An empty port is not one.
fn ssh_port_valid(port: &str, services: &Services) -> bool {
    matches!(ssh_a2port(port, services), Ok(Some(p)) if p > 0)
}

/// The port ssh reads from a written one, for `Port` and for a jump host
/// alike (`a2port` in misc.c): a number from 0 to 65535 after optional
/// ASCII whitespace and a sign, as `strtonum` reads it (`ssh -G`: `Port "
/// 2200"`, `"\t2200"` and `" +2200"` are 2200, `+2` and `02` are 2, `-0` is
/// 0; `"2200 "` and `"+ 2200"` are not numbers), or else the service name
/// the system knows by the text as written. `Ok(None)` when it is neither;
/// `Err` when the services file cannot be read. ssh refuses 0, as it does a
/// name it does not know.
fn ssh_a2port(port: &str, services: &Services) -> Result<Option<u16>, String> {
    let number = port.trim_start_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
    let negative = number.starts_with('-');
    let digits = number.strip_prefix(['+', '-']).unwrap_or(number);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        let value = digits.trim_start_matches('0');
        let value = if value.len() > 5 {
            u32::MAX
        } else {
            value.parse().unwrap_or(0)
        };
        // In range for `strtonum` (0 to 65535): that number is the port.
        if value == 0 || (!negative && value <= 65535) {
            return Ok(Some(value as u16));
        }
    }
    services.port(port)
}

/// Whether ssh decodes the user name of an `ssh://` jump host (`urldecode`
/// in misc.c): every `%` followed by two hex digits that do not make 0.
fn ssh_url_decodes(s: &str) -> bool {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            i += 1;
            continue;
        }
        match bytes.get(i + 1..i + 3) {
            Some([a, b])
                if a.is_ascii_hexdigit() && b.is_ascii_hexdigit() && (*a, *b) != (b'0', b'0') =>
            {
                i += 3
            }
            _ => return false,
        }
    }
    true
}

/// The ports of the service names a file names, each looked up once
/// ([`service_port`]): a file may repeat a name on every line.
#[derive(Default)]
struct Services(
    std::cell::RefCell<HashMap<String, Result<Option<u16>, String>>>,
    #[cfg(not(unix))] std::cell::OnceCell<Result<String, String>>,
);

impl Services {
    fn port(&self, name: &str) -> Result<Option<u16>, String> {
        if let Some(found) = self.0.borrow().get(name) {
            return found.clone();
        }
        #[cfg(unix)]
        let found = service_port(name);
        // The file is read once for the import, whatever the number of
        // names a file gives (`ssh -G` reads it for each, a few KiB).
        #[cfg(not(unix))]
        let found = self
            .1
            .get_or_init(services_file)
            .as_ref()
            .map_err(Clone::clone)
            .map(|text| service_in(text, name));
        self.0.borrow_mut().insert(name.to_string(), found.clone());
        found
    }
}

/// The port of a service name for `tcp`, as ssh looks it up
/// (`getservbyname`, case included: `Port ssh` is 22, `Port Ssh` is not
/// known), or `None` when the system does not know it. On Windows, where
/// the system reads the services file, that file is read once
/// ([`services_file`], [`service_in`]).
#[cfg(unix)]
fn service_port(name: &str) -> Result<Option<u16>, String> {
    // `getservbyname` returns a static entry: one lookup at a time, read
    // before the next. Nothing else in AeroFTP calls it.
    static LOOKUP: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let Ok(name) = std::ffi::CString::new(name) else {
        return Ok(None);
    };
    let _one = LOOKUP.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: both strings end in NUL and outlive the call; the entry
    // is read while the lock is held, before any other lookup.
    let entry = unsafe { libc::getservbyname(name.as_ptr(), c"tcp".as_ptr()) };
    // SAFETY: a non-null entry points to a valid `servent`.
    Ok((!entry.is_null()).then(|| u16::from_be(unsafe { (*entry).s_port } as u16)))
}

/// The Windows services file, or why it cannot be read: the host whose port
/// names a service is skipped with it.
#[cfg(not(unix))]
fn services_file() -> Result<String, String> {
    let path = std::path::PathBuf::from(
        std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".to_string()),
    )
    .join(r"System32\drivers\etc\services");
    std::fs::read_to_string(&path).map_err(|e| {
        format!("its port is a service name, and the services file could not be read ({e}); write the port number and import again")
    })
}

/// The `tcp` port a services file gives the name `name` or one of its
/// aliases, or `None`.
#[cfg_attr(unix, allow(dead_code))]
fn service_in(services: &str, name: &str) -> Option<u16> {
    services.lines().find_map(|line| {
        let mut fields = line.split('#').next()?.split_whitespace();
        let service = fields.next()?;
        let (port, protocol) = fields.next()?.split_once('/')?;
        let named = service == name || fields.any(|alias| alias == name);
        (named && protocol == "tcp").then(|| port.parse::<u16>().ok())?
    })
}

/// Maximum number of `Host` and `Match` blocks in a file, matching the other
/// importers: each imported profile with a credential costs a whole-vault
/// rewrite, and each host is resolved against every `Match` and wildcard
/// block, so the cap bounds both. A larger file is refused whole: its hosts
/// cannot be resolved against rules that were not read.
const MAX_HOSTS: usize = 10_000;

/// Maximum number of hosts times `Match` and wildcard `Host` blocks in a
/// file: each host is read against each of those blocks, so this bounds the
/// work of one import before it starts ([`SshConfig::too_much_work`]). A
/// file over it is refused whole, with the reason.
const MAX_RULE_CHECKS: usize = 2_000_000;

/// Maximum number of steps an import spends matching hosts against blocks:
/// one per block a host is read against and one per byte compared, so a
/// file whose rules pass [`MAX_RULE_CHECKS`] but whose host lists overlap or
/// whose patterns are long is still bounded ([`SshConfig::charge`]). A file
/// over it is refused whole, with the reason.
const MAX_MATCH_STEPS: u64 = 200_000_000;

/// What an import over one of its limits tells the user: a file split in
/// two loses the rules each half does not hold, so the advice keeps them.
const SMALLER_FILE: &str = "import a copy of the file that keeps the hosts you need and, in their order, every block that applies to them";

/// Parse the ssh config into its blocks, in file order: the lines before the
/// first `Host` or `Match` (when there are any), then one block per `Host`
/// and `Match` line. Comments (`#`) and blank lines are ignored. The second
/// value is the first line for which ssh refuses the whole file
/// ([`ssh_line_error`], or a quote left open), wherever it sits, or the
/// reason the file has more than [`MAX_HOSTS`] blocks.
#[cfg(test)]
fn parse_ssh_config(content: &str) -> (Vec<HostBlock>, Option<String>) {
    parse_ssh_config_with(content, &Services::default())
}

/// [`parse_ssh_config`], looking service names up in `services`. Only the
/// keys the import reads ([`PROFILE_KEYS`], [`RESOLUTION_KEYS`]) are kept:
/// a block of other lines costs nothing to apply.
fn parse_ssh_config_with(content: &str, services: &Services) -> (Vec<HostBlock>, Option<String>) {
    let mut blocks: Vec<HostBlock> = Vec::new();
    let mut current: Option<HostBlock> = Some(HostBlock {
        scope: Scope::Global,
        aliases: Vec::new(),
        kv: HashMap::new(),
        identities: Vec::new(),
        include: None,
    });
    let mut texts: HashMap<String, u32> = HashMap::new();
    let mut error: Option<String> = None;
    let mut hosts = 0;
    let mut capped = false;
    let push = |blocks: &mut Vec<HostBlock>, block: HostBlock| {
        if block.scope != Scope::Global
            || !block.kv.is_empty()
            || !block.identities.is_empty()
            || block.include.is_some()
        {
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
            Ok((key, words)) => ssh_line_error(key, words, services),
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
            if hosts >= MAX_HOSTS {
                if !capped {
                    log::warn!("ssh config import: refused past the {MAX_HOSTS}-block cap");
                    error.get_or_insert(format!(
                        "the file has more than {MAX_HOSTS} `Host` and `Match` blocks, more than one import reads; {SMALLER_FILE}"
                    ));
                }
                capped = true;
                continue;
            }
            hosts += 1;
            let scope = match key.as_str() {
                "host" => Scope::Host,
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
                identities: Vec::new(),
                include: None,
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
            if key == "include" {
                block.include.get_or_insert(index + 1);
                continue;
            }
            let value = match key.as_str() {
                "proxycommand" => words.join(" "),
                _ => words.into_iter().next().unwrap_or_default(),
            };
            if !PROFILE_KEYS.contains(&key.as_str()) && !RESOLUTION_KEYS.contains(&key.as_str()) {
                continue;
            }
            // ssh skips `IdentityFile none`, in any case, when it loads the
            // keys (`load_public_identity_files` in ssh.c): the key a host
            // tries first is its first other one, from any block. It still
            // counts the line.
            if key == "identityfile" {
                let next = texts.len() as u32;
                block
                    .identities
                    .push(*texts.entry(value.clone()).or_insert(next));
                if value.eq_ignore_ascii_case("none") {
                    continue;
                }
            }
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
/// bytes, `?` for one byte, anything else for itself, case included
/// (`ssh -G`: `Host A` applies to `ssh A`, not to `ssh a`; `Host ?` does not
/// apply to `ssh é`, two bytes in UTF-8, and `Host ??` does).
fn ssh_pattern(pattern: &str, name: &str, steps: &Steps) -> bool {
    ssh_pattern_in(pattern, name, false, steps)
}

/// [`ssh_pattern`], comparing ASCII letters in any case when `any_case`.
/// Each byte compared is a step ([`Steps`]); past the budget the pattern
/// does not match, and the import is refused.
fn ssh_pattern_in(pattern: &str, name: &str, any_case: bool, steps: &Steps) -> bool {
    let same = |a: u8, b: u8| a == b || (any_case && a.eq_ignore_ascii_case(&b));
    // Reading the pattern once, for its wildcards and its tail.
    steps.charge(1 + pattern.len() as u64);
    if !pattern.contains(['*', '?']) {
        return pattern.len() == name.len()
            && pattern.bytes().zip(name.bytes()).all(|(a, b)| same(a, b));
    }
    let (p, n) = (pattern.as_bytes(), name.as_bytes());
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    // A `*` may make each byte of the name be compared again for each byte
    // of the pattern: counted here, in one sum for the call.
    let mut count = 0u64;
    let left = steps.left();
    while ni < n.len() {
        count += 1;
        if count > left {
            steps.charge(count);
            return false;
        }
        // A `*` in the pattern is a wildcard before it is a byte to
        // compare (`match_pattern` in match.c: `ssh -G` with `User
        // "*buildsvc"` applies `Match user *svc`).
        if pi < p.len() && p[pi] == b'*' {
            star = Some((pi, ni));
            pi += 1;
        } else if pi < p.len() && (p[pi] == b'?' || same(p[pi], n[ni])) {
            pi += 1;
            ni += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            steps.charge(count);
            return false;
        }
    }
    steps.charge(count);
    p[pi..].iter().all(|&c| c == b'*')
}

/// The steps an import has spent matching ([`MAX_MATCH_STEPS`]).
#[derive(Default)]
struct Steps(std::cell::Cell<u64>);

impl Steps {
    fn charge(&self, steps: u64) {
        self.0.set(self.0.get().saturating_add(steps));
    }

    fn left(&self) -> u64 {
        MAX_MATCH_STEPS.saturating_sub(self.0.get())
    }

    fn spent(&self) -> bool {
        self.0.get() > MAX_MATCH_STEPS
    }
}

/// Whether a `Host` line applies to `alias`, as ssh decides it: one of its
/// patterns matches and none of its `!` patterns does (`ssh -G`: `Host * !b`
/// skips `b`, and `Host !c` alone applies to no host at all).
fn host_matches(patterns: &[String], alias: &str, steps: &Steps) -> bool {
    let mut matched = false;
    for pattern in patterns {
        match pattern.strip_prefix('!') {
            Some(negated) if ssh_pattern(negated, alias, steps) => return false,
            Some(_) => {}
            None => matched |= ssh_pattern(pattern, alias, steps),
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
    /// The local account, read once for the whole import.
    account: Account,
    /// The service names looked up so far.
    services: Services,
    /// The work spent so far ([`MAX_MATCH_STEPS`]).
    steps: Steps,
}

impl SshConfig {
    #[cfg(test)]
    fn new(blocks: Vec<HostBlock>) -> Self {
        Self::with(blocks, Services::default())
    }

    fn with(blocks: Vec<HostBlock>, services: Services) -> Self {
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
            // A name twice on one line applies the block once, as ssh
            // reads a `Host` line (`ssh -G`: `Host lab lab` with 100
            // `IdentityFile` lines is read; visited twice, the block would
            // count 200).
            for alias in &block.aliases {
                let own = by_name.entry(alias.clone()).or_default();
                if own.last() != Some(&index) {
                    own.push(index);
                }
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
            account: local_account(),
            services,
            steps: Steps::default(),
        }
    }

    /// The blocks that may apply to `alias`, in file order, each with
    /// whether it is one of the blocks that name `alias` itself: those
    /// apply to it without matching their patterns again (each of them is
    /// a name, `alias` among them). Each block is a step.
    fn candidates(&self, alias: &str) -> Vec<(&HostBlock, bool)> {
        // Both lists are in file order: merged, not sorted again per host.
        let own: &[usize] = self.by_name.get(alias).map_or(&[], Vec::as_slice);
        self.steps.charge((self.others.len() + own.len()) as u64);
        let (mut others, mut own) = (self.others.iter().peekable(), own.iter().peekable());
        let mut blocks = Vec::with_capacity(self.others.len() + own.len());
        loop {
            let next = match (others.peek(), own.peek()) {
                (Some(a), Some(b)) if a <= b => others.next().map(|i| (i, false)),
                (_, Some(_)) => own.next().map(|i| (i, true)),
                (Some(_), None) => others.next().map(|i| (i, false)),
                (None, None) => break,
            };
            blocks.extend(next.map(|(i, own)| (&self.blocks[*i], own)));
        }
        blocks
    }

    /// Why the import stopped, once it has spent [`MAX_MATCH_STEPS`].
    fn over_budget(&self) -> Option<String> {
        self.steps.spent().then(|| {
            format!("matching the file's hosts against its `Host` and `Match` blocks takes more than {MAX_MATCH_STEPS} steps, more than one import spends; {SMALLER_FILE}")
        })
    }

    /// Why the file is not resolved: more host-against-rule checks than
    /// [`MAX_RULE_CHECKS`], each host being read against every `Match` and
    /// wildcard block.
    fn too_much_work(&self) -> Option<String> {
        let hosts = self
            .blocks
            .iter()
            .filter(|b| b.scope == Scope::Host)
            .count();
        let rules = self
            .others
            .iter()
            .filter(|i| self.blocks[**i].scope != Scope::Global)
            .count();
        (hosts.saturating_mul(rules) > MAX_RULE_CHECKS).then(|| {
            format!("the file reads each of its {hosts} hosts against {rules} `Match` and wildcard `Host` blocks, more than one import resolves; {SMALLER_FILE}")

        })
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

/// The keys that decide whether ssh reads the file again, or asks DNS for
/// the host, before it settles the values of [`PROFILE_KEYS`].
const RESOLUTION_KEYS: [&str; 3] = [
    "canonicalizehostname",
    "canonicaldomains",
    "canonicalizepermittedcnames",
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
/// port 2345); the host name is the one read in the first, and a `Match` on
/// a condition known only when ssh runs that may turn this reading on skips
/// the host. With `CanonicalDomains` too, or a host name ending in `.`, ssh
/// may rename the host through DNS before that reading, which the import
/// cannot know: the host is skipped. The file's `Include` lines are not read.
fn host_values(config: &SshConfig, alias: &str) -> Result<HashMap<String, String>, String> {
    let mut values: HashMap<String, String> = HashMap::new();
    let mut identities = Identities::default();
    read_pass(config, &mut values, &mut identities, alias, alias, false)?;
    // `true` is `yes` to ssh (`ssh -G`: `CanonicalizeHostname true` reads
    // the file again for the host name).
    let canonicalize = values.get("canonicalizehostname").is_some_and(|v| {
        ["yes", "true", "always"]
            .iter()
            .any(|on| v.eq_ignore_ascii_case(on))
    });
    // A name ending in `.` is looked up too, domains or not (`ssh -vvv`:
    // "name is fully qualified", then `localhost.` => `localhost`), and a
    // CNAME the rules permit is followed (`ssh -G`: `www.github.com` with
    // `CanonicalizePermittedCNAMEs *:*` is read again as `github.com`; with
    // `none`, or `CanonicalizeHostname` off, it is not).
    // `none`, which stands alone ([`ssh_line_error`]), is no rule and no
    // domain (`ssh -G`: `CanonicalDomains none` still reads the file again,
    // and asks DNS for nothing).
    let set = |key: &str| {
        values
            .get(key)
            .is_some_and(|v| !v.eq_ignore_ascii_case("none"))
    };
    if canonicalize
        && (set("canonicaldomains")
            || set("canonicalizepermittedcnames")
            || match_host(&values, alias, config).ends_with('.'))
    {
        // Turning canonicalization off, or writing the host name, would
        // drop what the second reading gives the host: the advice keeps it.
        return Err("ssh may rename its host through DNS (`CanonicalizeHostname` with `CanonicalDomains` or `CanonicalizePermittedCNAMEs`, or a host name ending in `.`) and read the file again for the new name, which this import cannot know; to import it, import a copy of the file that holds only a `Host` block for it, with its full host name in `HostName` and the `Port`, `User` and `IdentityFile` ssh uses for it, and no `CanonicalizeHostname`, `CanonicalDomains` or `CanonicalizePermittedCNAMEs` line".to_string());
    }
    if canonicalize || config.second_pass {
        let host = ssh_host_name(&match_host(&values, alias, config));
        read_pass(config, &mut values, &mut identities, &host, alias, true)?;
    }
    Ok(values)
}

/// The `IdentityFile` lines that apply to a host, as ssh counts them: the
/// keys of the blocks that apply (`certain`), and those of `Match` blocks
/// on a condition known only when ssh runs, which may apply too (`maybe`,
/// with the line of the first such block).
#[derive(Default)]
struct Identities {
    certain: Vec<u32>,
    maybe: Vec<u32>,
    maybe_line: Option<usize>,
}

impl Identities {
    /// Whether adding `keys` to `known` makes ssh refuse the host: ssh
    /// counts each key before it drops a repeat, and stops at
    /// [`MAX_IDENTITY_FILES`] (`add_identity_file` in readconf.c). Each key
    /// is a step; the list compared stays at most that long.
    fn overflow(known: &[u32], keys: &[u32], steps: &Steps) -> bool {
        steps.charge(keys.len() as u64);
        if steps.spent() {
            return false;
        }
        let mut seen: Vec<u32> = known.to_vec();
        for key in keys {
            if seen.len() >= MAX_IDENTITY_FILES {
                return true;
            }
            if !seen.contains(key) {
                seen.push(*key);
            }
        }
        false
    }

    /// The keys that apply for certain, then those that may.
    fn all(&self) -> Vec<u32> {
        let mut all = self.certain.clone();
        all.extend(self.maybe.iter().filter(|k| !self.certain.contains(k)));
        all
    }
}

/// One reading of the file for `alias`, with `Host` patterns matched against
/// `name` (the alias, then the host name in the second reading).
/// Why a host that an `Include` applies to is skipped: ssh reads the
/// included files there as part of the file (`Include` in readconf.c, and
/// `ssh -G`: a `ProxyJump bastion` in an included file before `Host lab`
/// gives `lab` that jump), and the import does not open them, so any value
/// it reads for the host, a jump host among them, may not be the one ssh
/// uses. An `Include` in a block that does not apply sets nothing.
fn include_reason(line: usize, how: &str) -> String {
    format!("the `Include` on line {line} of the file {how}, and an import does not read the files it names, which may set the host's values, a jump host among them; import a copy of the file with the included lines in place of the `Include`")
}

fn read_pass(
    config: &SshConfig,
    values: &mut HashMap<String, String>,
    identities: &mut Identities,
    name: &str,
    alias: &str,
    second: bool,
) -> Result<(), String> {
    for (block, own) in config.candidates(name) {
        if let Some(reason) = config.over_budget() {
            return Err(reason);
        }
        let applies = match &block.scope {
            Scope::Global => true,
            Scope::Host => own || host_matches(&block.aliases, name, &config.steps),
            Scope::Match(line) => match match_holds(line, values, alias, second, config) {
                Some(holds) => holds,
                None => {
                    if let Some(at) = block.include {
                        return Err(include_reason(at, "may apply to it"));
                    }
                    // ssh settles `HostName` before the second reading, and
                    // asks DNS before it: the keys that decide those do
                    // nothing in it.
                    let unset = PROFILE_KEYS.iter().chain(&RESOLUTION_KEYS).find(|key| {
                        !(second && (**key == "hostname" || RESOLUTION_KEYS.contains(*key)))
                            && block.kv.contains_key(**key)
                            && !values.contains_key(**key)
                            && !(PROXY_KEYS.contains(*key)
                                && PROXY_KEYS.iter().any(|k| values.contains_key(*k)))
                    });
                    // A value set after that line would come too late:
                    // the first one ssh reads wins.
                    if let Some(key) = unset {
                        return Err(format!(
                            "the `Match` block on line {} of the file may set its `{key}`, on a condition known only when ssh runs; give the host its `{key}` in a `Host` block above that line and import again",
                            line.line
                        ));
                    }
                    // Its keys count when it applies, whichever key the
                    // host tries first.
                    if !block.identities.is_empty() {
                        if Identities::overflow(&identities.all(), &block.identities, &config.steps)
                        {
                            return Err(format!("the `Match` block on line {} of the file may add `IdentityFile` lines past the {MAX_IDENTITY_FILES} ssh reads, on a condition known only when ssh runs, and ssh then refuses to connect; give the host fewer `IdentityFile` lines and import again", line.line));
                        }
                        identities.maybe_line.get_or_insert(line.line);
                        for key in &block.identities {
                            if !identities.maybe.contains(key)
                                && identities.maybe.len() < MAX_IDENTITY_FILES
                            {
                                identities.maybe.push(*key);
                            }
                        }
                    }
                    false
                }
            },
        };
        if applies {
            if let Some(at) = block.include {
                return Err(include_reason(at, "applies to it"));
            }
            // ssh counts every line, `none` and repeats included, and keeps
            // a key once (`ssh -G`: 100 lines and a `Match final` fail in
            // the second reading, 150 lines of 50 keys do not). Each line is
            // a step: a block applies to every host it names.
            config.steps.charge(block.identities.len() as u64);
            if let Some(reason) = config.over_budget() {
                return Err(reason);
            }
            if let Some(line) = identities.maybe_line {
                if Identities::overflow(&identities.all(), &block.identities, &config.steps) {
                    return Err(format!("the `Match` block on line {line} of the file may add `IdentityFile` lines past the {MAX_IDENTITY_FILES} ssh reads, on a condition known only when ssh runs, and ssh then refuses to connect; give the host fewer `IdentityFile` lines and import again"));
                }
            }
            for identity in &block.identities {
                if identities.certain.len() >= MAX_IDENTITY_FILES {
                    return Err(format!("more than {MAX_IDENTITY_FILES} `IdentityFile` lines apply to it, and ssh refuses to connect with more"));
                }
                if !identities.certain.contains(identity) {
                    identities.certain.push(*identity);
                }
            }
            for (key, value) in &block.kv {
                // ssh fixes the host name before the second reading: a
                // `HostName` there does not change it (`ssh -G`: `Host a`,
                // then `Match final` / `HostName b`, keeps `a`).
                if second && key == "hostname" {
                    continue;
                }
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

/// The host name a `Match host` compares: the `HostName` read so far with
/// its `%h` expanded, or else the alias. The second reading matches `Host`
/// patterns against its [`ssh_host_name`].
fn match_host(values: &HashMap<String, String>, alias: &str, config: &SshConfig) -> String {
    let tokens = SshTokens {
        alias,
        host: alias,
        user: None,
        port: 22,
        account: &config.account,
        steps: &config.steps,
    };
    values
        .get("hostname")
        .filter(|h| !h.is_empty())
        .and_then(|h| ssh_expand("host", h, "h", &tokens, false).ok())
        .unwrap_or_else(|| alias.to_string())
}

/// The host name ssh connects to and reads the file again for (`main` in
/// ssh.c): a numeric address in its canonical form, kept as written when
/// that differs only in case; any other name with a `%` or a `:`, or of
/// digits and dots alone, as written (`is_addr_fast`); and the rest
/// lowercase in ASCII (`ssh -G` on 10.2p1: `HostName 2001:db8:0:0:0:0:0:1`
/// is `2001:db8::1`, `127.1` and `0x7f.1` are `127.0.0.1`, `2001:DB8::1`,
/// `AB:CD` and `fe80::1%LabLink` stay, `fe80:0::1%lo` is `fe80::1%lo`,
/// `ÄB.De` is `Äb.de`). The first reading compares the name as written.
fn ssh_host_name(name: &str) -> String {
    let address_like =
        name.contains(['%', ':']) || name.bytes().all(|b| b.is_ascii_digit() || b == b'.');
    match ssh_address(name) {
        Some(canonical) if !canonical.eq_ignore_ascii_case(name) => canonical,
        Some(_) => name.to_string(),
        None if address_like => name.to_string(),
        None => name.to_ascii_lowercase(),
    }
}

/// The canonical form of an address with a scope (`fe80::1%lo`), as the
/// system reads it (`getaddrinfo` with `AI_NUMERICHOST`, then
/// `getnameinfo` with `NI_NUMERICHOST`, as `resolve_addr` in ssh.c does):
/// which scopes it takes, and how it writes them, depends on the system and
/// its interfaces (`ssh -G` on Linux: `fe80::1%1` is `fe80::1%lo`,
/// `2001:db8::1%lo` is not an address). `None` when it is not one. Off
/// Unix, the name is kept as written.
fn scoped_address(name: &str) -> Option<String> {
    #[cfg(unix)]
    {
        let c_name = std::ffi::CString::new(name).ok()?;
        // SAFETY: zeroed `addrinfo` is a valid hints value.
        let mut hints: libc::addrinfo = unsafe { std::mem::zeroed() };
        hints.ai_flags = libc::AI_NUMERICHOST;
        hints.ai_family = libc::AF_UNSPEC;
        hints.ai_socktype = libc::SOCK_STREAM;
        let mut found: *mut libc::addrinfo = std::ptr::null_mut();
        // SAFETY: the strings and hints outlive the call; `found` is freed
        // below whenever the call succeeds.
        if unsafe { libc::getaddrinfo(c_name.as_ptr(), std::ptr::null(), &hints, &mut found) } != 0
            || found.is_null()
        {
            return None;
        }
        let mut host = [0 as libc::c_char; 1025];
        // SAFETY: `found` is a valid list from `getaddrinfo`, and `host` is
        // as long as the length given.
        let rc = unsafe {
            libc::getnameinfo(
                (*found).ai_addr,
                (*found).ai_addrlen,
                host.as_mut_ptr(),
                host.len() as libc::socklen_t,
                std::ptr::null_mut(),
                0,
                libc::NI_NUMERICHOST,
            )
        };
        // SAFETY: `found` came from `getaddrinfo` and is freed once.
        unsafe { libc::freeaddrinfo(found) };
        // SAFETY: on success `host` holds a NUL-terminated string.
        (rc == 0).then(|| {
            unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }
                .to_string_lossy()
                .into_owned()
        })
    }
    #[cfg(not(unix))]
    {
        let _ = name;
        None
    }
}

/// The canonical form of a numeric address as the system resolver reads it
/// (`getaddrinfo` with `AI_NUMERICHOST`): IPv6, or IPv4 in the forms
/// `inet_aton` takes (one to four parts, each decimal, octal with a leading
/// `0` or hex with `0x`), or one with a scope ([`scoped_address`]). `None`
/// for a name.
fn ssh_address(name: &str) -> Option<String> {
    if name.contains('%') {
        return scoped_address(name);
    }
    if let Ok(v6) = name.parse::<std::net::Ipv6Addr>() {
        return Some(v6.to_string());
    }
    let parts: Vec<u64> = name
        .split('.')
        .map(|part| {
            let (digits, radix) = match part.as_bytes() {
                [b'0', b'x' | b'X', ..] => (&part[2..], 16),
                [b'0', _, ..] => (&part[1..], 8),
                _ => (part, 10),
            };
            (!digits.is_empty() && digits.chars().all(|c| c.is_digit(radix)))
                .then(|| u64::from_str_radix(digits, radix).ok())
                .flatten()
        })
        .collect::<Option<_>>()?;
    let (last, head) = parts.split_last()?;
    if head.len() > 3 || head.iter().any(|p| *p > 255) {
        return None;
    }
    let width = 8 * (4 - head.len() as u32);
    if width < 64 && *last >= 1u64 << width {
        return None;
    }
    let value = head
        .iter()
        .fold(0u64, |acc, p| (acc << 8) | p)
        .checked_shl(width)?
        | last;
    Some(std::net::Ipv4Addr::from(u32::try_from(value).ok()?).to_string())
}

/// Whether a `Match` line holds for `alias` with the values read so far:
/// `Some` when every condition it needs is known from the file, `None` when
/// one is known only when ssh runs and none of the others is false.
fn match_holds(
    line: &MatchLine,
    values: &HashMap<String, String>,
    alias: &str,
    second: bool,
    config: &SshConfig,
) -> Option<bool> {
    let steps = &config.steps;
    let mut known = true;
    for (negated, criterion) in &line.criteria {
        // Each condition read is a step, whatever it costs to decide.
        steps.charge(1);
        let holds = match criterion {
            Criterion::All => Some(true),
            Criterion::Canonical | Criterion::Final => Some(second),
            Criterion::Host(patterns) => {
                let host = match_host(values, alias, config);
                let host = if second { ssh_host_name(&host) } else { host };
                Some(pattern_list_matches(patterns, &host, true, steps))
            }
            Criterion::OriginalHost(patterns) => {
                Some(pattern_list_matches(patterns, alias, true, steps))
            }
            Criterion::User(patterns) => values
                .get("user")
                .or(config.account.0.as_ref())
                .map(|user| pattern_list_matches(patterns, user, false, steps)),
            Criterion::LocalUser(patterns) => config
                .account
                .0
                .as_ref()
                .map(|user| pattern_list_matches(patterns, user, false, steps)),
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
/// The length at which ssh fails a pattern list ([`pattern_list_matches`]).
const MAX_SUBPATTERN: usize = 1023;

fn pattern_list_matches(list: &str, name: &str, any_case: bool, steps: &Steps) -> bool {
    let mut matched = false;
    for pattern in list.split(',') {
        // ssh copies each subpattern into a 1024-byte buffer and fails the
        // whole list at one of 1023 bytes or more after its `!` (`sub` in
        // `match_pattern_list`, match.c; `ssh -G`: `Match originalhost
        // lab,<1023 x>` does not apply to `lab`, with 1022 it does).
        if pattern.strip_prefix('!').unwrap_or(pattern).len() >= MAX_SUBPATTERN {
            return false;
        }
        match pattern.strip_prefix('!') {
            Some(negated) if ssh_pattern_in(negated, name, any_case, steps) => return false,
            Some(_) => {}
            None => matched |= ssh_pattern_in(pattern, name, any_case, steps),
        }
    }
    matched
}

/// The host a `Host` block is imported as: its first pattern that names one
/// host, neither a wildcard nor a `!` exclusion, and that the line applies
/// to (`ssh -G`: `Host !old old backup` applies to `backup`, not to `old`).
fn profile_alias(block: &HostBlock, steps: &Steps) -> Result<String, String> {
    if block.aliases.iter().any(|a| is_wildcard_pattern(a)) {
        return Err("wildcard host pattern".to_string());
    }
    if block.aliases.iter().all(|a| a.is_empty()) {
        return Err("empty host alias".to_string());
    }
    let mut named = block
        .aliases
        .iter()
        .filter(|a| !a.starts_with('!'))
        .peekable();
    if named.peek().is_none() {
        return Err(
            "its `Host` line only excludes hosts, and ssh applies such a line to no host"
                .to_string(),
        );
    }
    named
        .find(|a| host_matches(&block.aliases, a, steps))
        .cloned()
        .ok_or_else(|| {
            "its `Host` line excludes every host it names, and ssh applies it to none of them"
                .to_string()
        })
}

/// Convert the `Host` block at `index` of the file to a profile, with the
/// values ssh reads for its host from every block ([`host_values`]), or the
/// reason it is skipped: a wildcard pattern, no usable alias, or a value
/// that cannot be imported.
fn map_host_block(config: &SshConfig, index: usize) -> Result<ServerProfileExport, String> {
    let alias = profile_alias(&config.blocks[index], &config.steps)?;
    let kv = &host_values(config, &alias)?;
    // A port outside 1 to 65535, or a service name the system does not
    // know, already refused the whole file.
    let port = match kv.get("port") {
        Some(p) => ssh_a2port(p, &config.services)?
            .filter(|p| *p > 0)
            .map(u32::from)
            .ok_or("its port is a service name the system does not know")?,
        None => 22,
    };
    // ssh expands `HostName` first (its `%h` is the alias), then the others
    // against that host name: the order the tokens are resolved in below.
    let mut tokens = SshTokens {
        alias: &alias,
        host: &alias,
        user: None,
        port,
        account: &config.account,
        steps: &config.steps,
    };
    // ssh lowercases the host name, `%h` included, in ASCII only (`ssh -vvv
    // A` with `IdentityFile /k/%h_%n` loads `/k/a_A`; `HostName ÄB.De` is
    // `Äb.de`).
    let host = ssh_host_name(&match kv.get("hostname").filter(|h| !h.is_empty()) {
        Some(h) => ssh_expand("host", h, "h", &tokens, false)?,
        None => alias.clone(),
    });
    tokens.host = &host;
    // With no `User`, ssh connects as the local account and expands `%r`
    // to its name (ssh.c sets the user before it expands `IdentityFile`),
    // so the profile takes that name too; when it cannot be read, `%r`
    // skips the host, never an empty name, and the profile has no user name.
    let username = match kv.get("user") {
        Some(u) => ssh_expand("user name", u, "dhnpu", &tokens, true)?,
        None => config.account.0.clone().unwrap_or_default(),
    };
    tokens.user = (kv.contains_key("user") || !username.is_empty()).then_some(username.as_str());

    let mut options = serde_json::Map::new();
    if let Some(identity) = kv.get("identityfile").filter(|s| !s.is_empty()) {
        // The `~` first, then the tokens, in ssh's order: a `~` a token or a
        // variable gives is not a home folder to ssh (`USER=~` with
        // `IdentityFile ${USER}/k` loads `~/k`, a folder called `~`).
        let identity = ssh_tilde(identity, config.account.1.as_deref(), passwd_home)?;
        let identity = ssh_expand("key file path", &identity, "dhnpru", &tokens, true)?;
        if identity.starts_with('~') {
            return Err("its key file path starts with `~` only once its tokens or variables are expanded, which ssh reads as a folder called `~`, not a home folder; write the path out and import again".to_string());
        }
        options.insert(
            "private_key_path".into(),
            serde_json::Value::String(identity),
        );
    }
    // `none` turns either off (`ssh -G`: `ProxyJump none`, any case, is no
    // jump; a `ProxyCommand none` first leaves out a later `ProxyJump`).
    if let Some(command) = kv
        .get("proxycommand")
        .filter(|c| !c.eq_ignore_ascii_case("none"))
    {
        let _ = command;
        return Err(
            "ssh reaches it through a `ProxyCommand`, which AeroFTP does not run".to_string(),
        );
    }
    // A connection opens no jump host: a profile that kept the jump would
    // connect straight to the host, which ssh does not do. `none # c` is a
    // jump through a host called `none` to ssh (see [`split_directive`]).
    if kv
        .get("proxyjump")
        .is_some_and(|s| !s.is_empty() && !s.eq_ignore_ascii_case("none"))
    {
        return Err(
            "ssh reaches it through a jump host (`ProxyJump`), which AeroFTP does not open"
                .to_string(),
        );
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

    let services = Services::default();
    let (blocks, error) = parse_ssh_config_with(&content, &services);
    let config = SshConfig::with(blocks, services);
    let error = error.or_else(|| config.too_much_work());
    let total_remotes = config
        .blocks
        .iter()
        .filter(|b| b.scope == Scope::Host)
        .count();
    // A file ssh refuses with no host block left to carry the reason (all
    // of them past the block cap, or none at all) is refused here: an empty
    // success would say it held nothing to import.
    if let (Some(reason), 0) = (&error, total_remotes) {
        return Err(reason.clone());
    }
    let (mut servers, mut skipped) = import_hosts(&config, error.as_ref());
    // Past the step budget the hosts read so far are dropped too: the file
    // is refused whole, as for the other limits.
    if let Some(reason) = config.over_budget() {
        log::warn!("ssh config import: refused past the {MAX_MATCH_STEPS}-step budget");
        (servers, skipped) = import_hosts(&config, Some(&reason));
    }

    Ok(SshImportResult {
        servers,
        skipped,
        source_path: path.display().to_string(),
        total_remotes,
    })
}

/// The profiles of the file's `Host` blocks and the blocks skipped, each
/// with its reason; with `error`, every block is skipped with it.
fn import_hosts(
    config: &SshConfig,
    error: Option<&String>,
) -> (Vec<ServerProfileExport>, Vec<SshSkippedRemote>) {
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
        let mapped = match error {
            Some(reason) => Err(reason.clone()),
            None if config.steps.spent() => continue,
            None => match profile_alias(block, &config.steps) {
                Ok(alias) if !seen.insert(alias.clone()) => Err(
                    "an earlier `Host` block names the same host, and ssh reads the two as one: it is imported once, from the first"
                        .to_string(),
                ),
                _ => map_host_block(config, index),
            },
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
    (servers, skipped)
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
    ssh_word_checked(field, value, dollar)?;
    Ok(value.replace('%', "%%"))
}

/// The refusals of [`ssh_word`], before its `%` is escaped.
fn ssh_word_checked(field: &str, value: &str, dollar: bool) -> Result<(), String> {
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
    Ok(())
}

/// A `ProxyJump` value as ssh reads it back (`ssh -G` and `ssh -vvv` on
/// 10.2p1): ssh reads the hops before it expands tokens, decoding the `%XX`
/// of an `ssh://` hop (`ssh://u%2B@j` is the user `u+`, and `u%%2B` makes
/// ssh refuse the file), then expands the tokens of the command it runs for
/// each hop (`u%%x@j` connects as `u%x`, `%h-j` to the host's name). So an
/// `ssh://` hop is written as it is, a `%` its encoding decodes to (`%25`)
/// is refused, since the command would read it as a token, and the `%` of
/// another hop is written `%%`. A value ssh would refuse, by the rules the
/// import reads it with ([`ssh_jump_valid`]), is refused too: one profile
/// would make ssh refuse every host of the file.
fn ssh_jump_word(value: &str) -> Result<String, String> {
    ssh_word_checked("jump host", value, false)?;
    let refused = |why: &str| format!("its jump host {why}; correct the profile and export again");
    // Each hop before the last reaches one more ssh down the chain, and each
    // expands the `%` tokens of the command it runs (`ssh -vvv`: `u%%x@a,b`
    // gives the first `-J u%x@a`, and the second stops at "unknown key %x").
    let earlier = value.rsplit_once(',').map_or("", |(earlier, _)| earlier);
    if earlier.contains('%') {
        return Err(refused(
            "holds `%` in a hop before the last, which each ssh down the chain reads as a token again",
        ));
    }
    let hops = value
        .split(',')
        .map(|hop| match hop.strip_prefix("ssh://") {
            Some(uri) if uri.contains("%25") => Err(refused(
                "encodes `%` as `%25`, which ssh decodes and then reads as a token in the command it runs for the jump",
            )),
            Some(_) => Ok(hop.to_string()),
            None => Ok(hop.replace('%', "%%")),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let written = hops.join(",");
    if written.contains('#') {
        return Err(refused(
            "holds `#`, which ssh would read as the start of a comment",
        ));
    }
    if !ssh_jump_valid(&written, &Services::default()) {
        return Err(refused(
            "is not one ssh reads, and ssh would refuse the whole file",
        ));
    }
    if !ssh_jump_reaches_no_shell(&written) {
        return Err(refused(
            "holds a character other than a letter, a digit or one of `._+-%@:/`, brackets other than around a whole IPv6 address in its last hop, or a name that starts with `-`, and ssh passes the jump host unquoted to a shell in the command it runs for it",
        ));
    }
    Ok(written)
}

/// Whether a written `ProxyJump` value, read by ssh, puts only plain words
/// in the command ssh runs for the jump (`ssh -vvv` on 10.2p1, "Setting
/// implicit ProxyCommand from ProxyJump"): ssh runs it through the shell
/// and inserts the last hop's user (`-l`, after decoding an `ssh://` hop's
/// `%XX`) and host unquoted, and the earlier hops as written (`-J`), so
/// `u$(cmd)@h`, ``u`cmd`@h``, `u;cmd@h`, `ssh://u%24%28cmd%29@h` and
/// `u@j$(cmd)` run `cmd`, and a host `-oProxyCommand=cmd` is read as an
/// option. Every name must hold only ASCII letters, digits and `._+-%`
/// (`@` in a user name) and not start with `-`; the last hop's host may be a
/// whole IPv6 literal `[...]` instead, whose brackets ssh drops; an earlier
/// hop, written whole into the command, only those and `:/@`, since `[` and
/// `]` are a shell glob there (`-J [::1]:1`, and `j[ab]` anywhere). A `%` in
/// an earlier hop never reaches it: [`ssh_jump_word`] refuses it first.
fn ssh_jump_reaches_no_shell(written: &str) -> bool {
    let plain = |s: &str, extra: &[u8]| {
        !s.is_empty()
            && !s.starts_with('-')
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-%".contains(&b) || extra.contains(&b))
    };
    let (earlier, last) = match written.rsplit_once(',') {
        Some((earlier, last)) => (Some(earlier), last),
        None => (None, written),
    };
    let earlier_plain =
        earlier.is_none_or(|earlier| earlier.split(',').all(|hop| plain(hop, b":/@")));
    let (user, rest) = match last.strip_prefix("ssh://") {
        Some(uri) => match uri.split_once('@') {
            Some((user, rest)) => {
                let user = user.split(';').next().unwrap_or("");
                (Some(ssh_url_decode(user)), rest)
            }
            None => (None, uri),
        },
        None => match last.rfind('@') {
            Some(at) => (Some(last[..at].to_string()), &last[at + 1..]),
            None => (None, last),
        },
    };
    let host = ssh_host_delim(rest).map_or(rest, |(host, _, _)| host);
    // ssh writes the last hop's host without its brackets (`[::1]` is `::1`
    // in the command), so a whole IPv6 literal is the one `[...]` read.
    let host_plain = match host.strip_prefix('[').and_then(|h| h.strip_suffix(']')) {
        Some(ip) => {
            !ip.is_empty()
                && ip
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() || b":.".contains(&b))
        }
        None => plain(host, b""),
    };
    earlier_plain && user.is_none_or(|user| plain(&user, b"@")) && host_plain
}

/// The text an `ssh://` user decodes to (`urldecode` in misc.c); a value
/// [`ssh_url_decodes`] refuses is never passed here.
fn ssh_url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (b, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
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
    // A key file path keeps the spaces at its ends: they are part of the
    // file's name (`IdentityFile "/keys/lab "` loads `/keys/lab `), and the
    // import keeps them.
    let key_file = raw("private_key_path")
        .filter(|k| !k.trim().is_empty())
        .map(|k| ssh_path("key file path", k))
        .transpose()?;
    // `ProxyJump` alone also ends its value at a `#` inside it (`ssh -G`:
    // `ProxyJump a#b` reads as `a`).
    let proxy = opt("proxyJump").map(ssh_jump_word).transpose()?;
    // ssh refuses the whole file for a port outside 1 to 65535 (`ssh -G`:
    // "Bad port '0'", "Bad port '65536'"), and the import with it.
    if !(1..=65535).contains(&server.port) {
        return Err(format!(
            "its port {} is not one from 1 to 65535, and ssh would refuse the whole file; correct the profile and export again",
            server.port
        ));
    }

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

    thread_local! {
        /// How many times this thread read the local account.
        pub(super) static ACCOUNT_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    const JUMP: &str =
        "ssh reaches it through a jump host (`ProxyJump`), which AeroFTP does not open";

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
            const NAME: &str = "test_default_path_is_the_accounts_not_home";
            // The child runs with `HOME` on a folder that has a config.
            if std::env::var("AEROFTP_SSH_IMPORT_TEST_CHILD").as_deref() == Ok(NAME) {
                let dir = PathBuf::from(std::env::var("HOME").unwrap());
                let in_dir = Some(dir.join(".ssh").join("config"));
                assert_eq!(ssh_config_in(Some(dir.display().to_string())), in_dir);
                assert_eq!(
                    ssh_config_in(Some(dir.join("none").display().to_string())),
                    None
                );
                let detected = default_ssh_config_path();
                assert_ne!(detected, in_dir);
                assert_eq!(detected, ssh_config_in(local_account().1));
                return;
            }
            let dir = tempfile::TempDir::new().unwrap();
            std::fs::create_dir_all(dir.path().join(".ssh")).unwrap();
            std::fs::write(dir.path().join(".ssh").join("config"), "Host x\n").unwrap();
            let home = dir.path().display().to_string();
            assert!(!in_child_with_env(NAME, &[("HOME", &home)]));
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

    /// Every exported value with a `%` reads back as the profile holds it,
    /// with ssh (`ssh -G` for the file, the host and the jump; `ssh -vvv` for
    /// the key it loads and the command it runs for a jump) and with the
    /// import: an `ssh://` jump keeps its `%XX` encoding, which ssh decodes
    /// before it reads tokens, a plain last hop and the other values go out
    /// with `%%`, and a jump ssh would read otherwise, or refuse, is refused,
    /// a `%` in a hop before the last among them.
    #[test]
    fn every_exported_percent_reads_back_as_the_profile_holds_it() {
        let none = serde_json::json!({});
        let jump = |j: &str| serde_json::json!({ "proxyJump": j });
        let servers = [
            sftp("addr", "fe80::1%eth0", "u", none.clone()),
            sftp(
                "key",
                "h.example.com",
                "u",
                serde_json::json!({"private_key_path": "/keys/id%u_%%"}),
            ),
            sftp(
                "uri",
                "h.example.com",
                "u",
                jump("ssh://u%2B@jump.example.com"),
            ),
            sftp("hop", "h.example.com", "u", jump("u%x@127.0.0.1:1")),
            sftp(
                "mixed",
                "h.example.com",
                "u",
                jump("a%b,ssh://c%2Bd@e.example.com"),
            ),
            sftp(
                "decoded",
                "h.example.com",
                "u",
                jump("ssh://u%25x@jump.example.com"),
            ),
            sftp(
                "double",
                "h.example.com",
                "u",
                jump("ssh://u%%2B@jump.example.com"),
            ),
            sftp(
                "host code",
                "h.example.com",
                "u",
                jump("ssh://j%2Eexample.com"),
            ),
        ];
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
        let skipped: Vec<(&str, &str)> = outcome
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                ("mixed", "its jump host holds `%` in a hop before the last, which each ssh down the chain reads as a token again; correct the profile and export again"),
                ("decoded", "its jump host encodes `%` as `%25`, which ssh decodes and then reads as a token in the command it runs for the jump; correct the profile and export again"),
                ("double", "its jump host is not one ssh reads, and ssh would refuse the whole file; correct the profile and export again"),
                ("host code", "its jump host is not one ssh reads, and ssh would refuse the whole file; correct the profile and export again"),
            ]
        );
        let config = std::fs::read_to_string(&path).unwrap();
        for line in [
            "  HostName fe80::1%%eth0\n",
            "  IdentityFile /keys/id%%u_%%%%\n",
            "  ProxyJump ssh://u%2B@jump.example.com\n",
            "  ProxyJump u%%x@127.0.0.1:1\n",
        ] {
            assert!(config.contains(line), "{line:?}\n{config}");
        }
        // The import reads each value back as the profile holds it; a host
        // reached through a jump is skipped, as for any file.
        let back = import_ssh_config(&path).unwrap();
        let by = |name: &str| back.servers.iter().find(|s| s.name == name);
        assert_eq!(by("addr").map(|s| s.host.as_str()), Some("fe80::1%eth0"));
        assert_eq!(
            by("key").map(|s| s.options.as_ref().unwrap()["private_key_path"].clone()),
            Some(serde_json::json!("/keys/id%u_%%"))
        );
        let jumped: Vec<&str> = back
            .skipped
            .iter()
            .filter(|s| s.reason == JUMP)
            .map(|s| s.name.as_str())
            .collect();
        assert_eq!(jumped, ["uri", "hop"]);
        // What ssh 10.2 reads, the reference: an earlier ssh or the Windows
        // port parses jump hosts and logs the command otherwise.
        if !ssh_major().is_some_and(|major| major >= 10) {
            eprintln!("SKIPPED: no ssh 10 or later here, so the file is not read back with it");
            return;
        }
        let g = |host: &str| ssh_g(&path, host);
        assert_eq!(g("addr")["hostname"], "fe80::1%eth0");
        assert_eq!(g("uri")["proxyjump"], "u+@jump.example.com");
        assert_eq!(
            ssh_first_key(&path, "key").as_deref(),
            Some("/keys/id%u_%%")
        );
        // The command ssh runs for the jump, its tokens expanded: the user
        // is `u%x`, as the profile holds it.
        if let Some(command) = ssh_jump_command(&path, "hop") {
            assert!(command.starts_with("exec ssh -l u%x -p 1 "), "{command}");
        }
    }

    /// A port outside 1 to 65535 is not written: ssh would refuse the whole
    /// file for it, its other hosts with it. The other profiles still go
    /// out, and an export with none left keeps the file at the destination.
    #[test]
    fn a_port_ssh_refuses_is_never_written() {
        let port = |name: &str, port: u32| {
            let mut server = sftp(name, "h.example.com", "u", serde_json::json!({}));
            server.port = port;
            server
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let servers = [port("zero", 0), port("ok", 2200), port("big", 65536)];
        let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
        let skipped: Vec<(&str, &str)> = outcome
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                ("zero", "its port 0 is not one from 1 to 65535, and ssh would refuse the whole file; correct the profile and export again"),
                ("big", "its port 65536 is not one from 1 to 65535, and ssh would refuse the whole file; correct the profile and export again"),
            ]
        );
        assert_eq!(outcome.exported, 1);
        let back = import_ssh_config(&path).unwrap();
        let read: Vec<(&str, u32)> = back
            .servers
            .iter()
            .map(|s| (s.name.as_str(), s.port))
            .collect();
        assert_eq!(read, [("ok", 2200)], "{:?}", reasons(&back));
        assert!(back.skipped.is_empty(), "{:?}", reasons(&back));
        std::fs::write(&path, "mine\n").unwrap();
        let outcome = export_ssh_config(
            &[port("zero", 0), port("big", 65536)],
            &HashMap::new(),
            &path,
        )
        .unwrap();
        assert_eq!(outcome.exported, 0);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "mine\n");
    }

    /// A key file path keeps the whitespace at its ends, part of the file's
    /// name: `IdentityFile "/keys/lab "` is imported as `/keys/lab `, and
    /// exported so that ssh, and the import, read `/keys/lab ` again.
    #[test]
    fn a_key_file_path_keeps_the_spaces_at_its_ends() {
        let dir = tempfile::tempdir().unwrap();
        let key = |result: &SshImportResult| {
            result.servers[0].options.as_ref().unwrap()["private_key_path"].clone()
        };
        let imported = import_text(
            dir.path(),
            "Host lab\n  HostName lab.example.com\n  IdentityFile \"/keys/lab \"\n",
        );
        assert_eq!(key(&imported), serde_json::json!("/keys/lab "));
        let server = sftp(
            "lab",
            "lab.example.com",
            "u",
            serde_json::json!({ "private_key_path": "/keys/lab " }),
        );
        let path = dir.path().join("exported");
        let outcome = export_ssh_config(&[server], &HashMap::new(), &path).unwrap();
        assert_eq!(outcome.exported, 1, "{:?}", outcome.skipped);
        assert_eq!(
            key(&import_ssh_config(&path).unwrap()),
            serde_json::json!("/keys/lab ")
        );
        if std::process::Command::new("ssh").arg("-V").output().is_ok() {
            assert_eq!(ssh_g(&path, "lab")["identityfile"], "/keys/lab ");
        }
    }

    /// ssh fails a pattern list with an entry of 1023 bytes or more after
    /// its `!`, the entries before it too: `Match originalhost lab,<1022
    /// bytes>` applies to `lab`, with 1023 it does not, negated or not.
    #[test]
    fn a_pattern_ssh_cannot_hold_matches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = std::process::Command::new("ssh").arg("-V").output().is_ok();
        for (len, bang, applies) in [
            (1022, "", true),
            (1023, "", false),
            (1022, "!", true),
            (1023, "!", false),
        ] {
            let text = format!(
                "Host lab\n  HostName lab.example.com\nMatch originalhost lab,{bang}{}\n  Port 2200\n",
                "x".repeat(len)
            );
            let result = import_text(dir.path(), &text);
            let port = result.servers[0].port;
            assert_eq!(port == 2200, applies, "{len}{bang}");
            if ssh {
                let read = ssh_g(&dir.path().join("config"), "lab");
                assert_eq!(read["port"], port.to_string(), "{len}{bang}");
            }
        }
    }

    /// ssh reads the files an `Include` names where the line is, so a
    /// `ProxyJump` in one reaches the hosts after it (`ssh -G`). The import
    /// does not open them: it skips every host an `Include` applies to, or
    /// may apply to, rather than import it with values ssh may not use, a
    /// direct connection where ssh goes through a jump host. An `Include` in
    /// a block that does not apply sets nothing.
    #[test]
    fn a_host_an_include_applies_to_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let extra = dir.path().join("extra");
        std::fs::write(&extra, "ProxyJump bastion.example.com\n").unwrap();
        let x = extra.display();
        let host = "Host lab\n  HostName lab.example.com\n";
        let reason = |line: usize, how: &str| {
            format!("the `Include` on line {line} of the file {how}, and an import does not read the files it names, which may set the host's values, a jump host among them; import a copy of the file with the included lines in place of the `Include`")
        };
        let cases = [
            (
                format!("Include {x}\n{host}"),
                vec![],
                vec![("lab", reason(1, "applies to it"))],
            ),
            (
                format!("{host}Host *\n  Include {x}\n"),
                vec![],
                vec![
                    ("lab", reason(4, "applies to it")),
                    ("*", "wildcard host pattern".to_string()),
                ],
            ),
            (
                format!("{host}Host other\n  Include {x}\n  HostName o.example.com\n"),
                vec!["lab"],
                vec![("other", reason(4, "applies to it"))],
            ),
            (
                format!("Match exec true\n  Include {x}\n{host}"),
                vec![],
                vec![("lab", reason(2, "may apply to it"))],
            ),
            (host.to_string(), vec!["lab"], vec![]),
        ];
        for (text, imported, skipped) in cases {
            let result = import_text(dir.path(), &text);
            let names: Vec<&str> = result.servers.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, imported, "{text}");
            let skipped: Vec<(&str, &str)> =
                skipped.iter().map(|(n, r)| (*n, r.as_str())).collect();
            assert_eq!(reasons(&result), skipped, "{text}");
        }
        if std::process::Command::new("ssh").arg("-V").output().is_ok() {
            let file = dir.path().join("included");
            std::fs::write(&file, format!("Include {x}\n{host}")).unwrap();
            assert_eq!(ssh_g(&file, "lab")["proxyjump"], "bastion.example.com");
        }
    }

    /// A host compares its `IdentityFile` keys by number, not by text: a
    /// file whose keys differ only in their last byte takes no longer than
    /// one of the same size whose keys differ in their first, where comparing
    /// the texts would read each of them whole for every pair (98 keys of
    /// 16 KiB after a short one, for each of 1000 hosts).
    #[test]
    fn comparing_keys_costs_the_same_whatever_their_length() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        let body = "k".repeat(16 * 1024);
        let config = |late: bool| {
            // The first key is short: each host expands the one it keeps.
            let mut text = String::from("IdentityFile /short\n");
            for i in 0..98 {
                let key = if late {
                    format!("/{body}{i:02}")
                } else {
                    format!("/{i:02}{body}")
                };
                text.push_str(&format!("IdentityFile {key}\n"));
            }
            for i in 0..1000 {
                text.push_str(&format!("Host h{i}\n  HostName h{i}.example.com\n"));
            }
            text
        };
        let time = |late: bool| {
            std::fs::write(&file, config(late)).unwrap();
            (0..3)
                .map(|_| {
                    let start = std::time::Instant::now();
                    let result = import_ssh_config(&file).unwrap();
                    assert_eq!(result.servers.len(), 1000, "{:?}", reasons(&result).first());
                    start.elapsed()
                })
                .min()
                .unwrap()
        };
        let early = time(false);
        let late = time(true);
        eprintln!("keys differing first: {early:?}, last: {late:?}");
        assert!(
            late < early * 3 + std::time::Duration::from_millis(50),
            "{late:?} against {early:?}"
        );
    }

    /// A jump host goes out only when every name ssh puts in the command it
    /// runs for the jump is a plain word: a shell character in a user or a
    /// host, plain, `%XX`-encoded or in an earlier hop, a name that starts
    /// with `-`, and an earlier `ssh://` hop holding `%` are refused, and
    /// the command ssh runs for each exported one holds the names as the
    /// profile holds them (`ssh -vvv` on 10.2p1).
    /// A variable name a global line reads is kept out of the reason every
    /// host it applies to is skipped with, past a short length, and reading
    /// it is charged to the step budget for each host.
    #[test]
    fn a_variable_name_is_not_copied_into_every_reason() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config");
        let name = "V".repeat(256 * 1024);
        let config = |hosts: usize| {
            let mut text = format!("User ${{{name}}}\n");
            for i in 0..hosts {
                text.push_str(&format!("Host h{i}\n  HostName h{i}.example.com\n"));
            }
            text
        };
        std::fs::write(&file, config(100)).unwrap();
        let result = import_ssh_config(&file).unwrap();
        assert_eq!(result.skipped.len(), 100);
        assert!(result.servers.is_empty());
        for skip in &result.skipped {
            assert_eq!(
                skip.reason,
                format!("its user name reads an environment variable whose name is longer than {MAX_REPORTED_NAME} bytes, which an import does not read; only {} are expanded", SSH_IMPORT_ENV.join(", "))
            );
        }
        // Past the budget, the file is refused whole.
        std::fs::write(&file, config(800)).unwrap();
        let result = import_ssh_config(&file).unwrap();
        assert!(result.servers.is_empty());
        assert_eq!(result.skipped.len(), 800);
        assert!(
            result.skipped.iter().all(|s| s.reason.contains("steps")),
            "{:?}",
            result.skipped.first().map(|s| &s.reason)
        );
        // A short name is still named.
        std::fs::write(&file, "User ${NOT_READ}\nHost h\n").unwrap();
        let result = import_ssh_config(&file).unwrap();
        assert!(result.skipped[0].reason.contains("`NOT_READ`"));
    }

    #[test]
    fn a_jump_host_never_reaches_a_shell() {
        let refused = [
            "u$(id)@j.example.com",
            "u`id`@j.example.com",
            "u;id@j.example.com",
            "u|id@j.example.com",
            "u&id@j.example.com",
            "u>x@j.example.com",
            "u<x@j.example.com",
            "u*@j.example.com",
            "u~@j.example.com",
            "u!x@j.example.com",
            "u(x)@j.example.com",
            "u{x}@j.example.com",
            "-oX@j.example.com",
            "u@-oProxyCommand=x",
            "u@j$(id)x",
            "ssh://u%24%28id%29@j.example.com",
            "ssh://u%3Bid@j.example.com",
            "ssh://u%60id%60@j.example.com",
            "ssh://%2DoX@j.example.com",
            "u$(id)@a.example.com,b.example.com",
            "ssh://u;x=y@a.example.com,b.example.com",
            "ssh://u%2B@a.example.com,b.example.com",
            "u@j[ab].example.com",
            "[::1]x",
            "[::1]:1,b.example.com",
            "u@[]:1",
            "u@[x]y",
            "u%x@a.example.com,b.example.com",
            "a%b.example.com,c.example.com",
        ];
        let exported = [
            ("plain", "u@127.0.0.1:1", "-l u -p 1 "),
            ("uri", "ssh://u%2B@127.0.0.1:1", "-l u+ -p 1 "),
            ("params", "ssh://u;x=y@127.0.0.1:1", "-l u -p 1 "),
            (
                "earlier",
                "a.example.com,ssh://c%2Bd@127.0.0.1:2",
                "-l c+d -p 2 -J a.example.com ",
            ),
            ("ipv6", "u@[::1]:1", "-l u -p 1 "),
            ("bare6", "[::1]", "-F "),
            (
                "chain",
                "a.example.com,u@127.0.0.1:2",
                "-l u -p 2 -J a.example.com ",
            ),
        ];
        let mut servers: Vec<_> = refused
            .iter()
            .enumerate()
            .map(|(i, j)| {
                sftp(
                    &format!("r{i}"),
                    "h.example.com",
                    "u",
                    serde_json::json!({ "proxyJump": j }),
                )
            })
            .collect();
        servers.extend(exported.iter().map(|(name, j, _)| {
            sftp(
                name,
                "127.0.0.1",
                "u",
                serde_json::json!({ "proxyJump": j }),
            )
        }));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config");
        let outcome = export_ssh_config(&servers, &HashMap::new(), &path).unwrap();
        let skipped: Vec<&str> = outcome.skipped.iter().map(|s| s.name.as_str()).collect();
        let expected: Vec<String> = (0..refused.len()).map(|i| format!("r{i}")).collect();
        assert_eq!(skipped, expected, "{:?}", outcome.skipped);
        assert_eq!(outcome.exported, exported.len());
        // What ssh 10.2 reads, the reference: an earlier ssh or the Windows
        // port parses jump hosts and logs the command otherwise.
        if !ssh_major().is_some_and(|major| major >= 10) {
            eprintln!("SKIPPED: no ssh 10 or later here, so the commands are not read with it");
            return;
        }
        for (name, _, args) in exported {
            let Some(command) = ssh_jump_command(&path, name) else {
                eprintln!("SKIPPED: no stand-in for ssh here, so the commands are not read");
                return;
            };
            let command = command
                .strip_prefix("exec ssh ")
                .unwrap_or_else(|| panic!("{name}: {command}"));
            assert!(command.starts_with(args), "{name}: {command}");
        }
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
             Match all\n  HostName %h.m.example.com\n\
             Host b\n  Port 2400\n",
        )
        .unwrap();
        let result = import_ssh_config(&config).unwrap();
        let me = local_account().0.unwrap_or_default();
        let me = me.as_str();
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
                ("a", "a.example.com", 22, "star", "/k/global", ""),
                ("b", "b.example.com", 2400, me, "/k/global", ""),
                ("ab", "ab.m.example.com", 2202, "star", "/k/global", ""),
                ("z", "z.m.example.com", 2202, "star", "/k/global", ""),
                ("c", "c.m.example.com", 22, "star", "/k/global", ""),
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
        // command skips its host, quoted or followed by a `#` (`ssh -G`
        // keeps the rest of the line as the command). `CanonicalizeHostname` reads the file a
        // second time; with `CanonicalDomains`, DNS may rename the host.
        let proxy = dir.path().join("proxy");
        std::fs::write(
            &proxy,
            "Host pc\n  ProxyCommand nc %h %p\n  ProxyJump j\n\
             Host pj\n  ProxyJump j\n  ProxyCommand nc %h %p\n\
             Host cy\n  CanonicalizeHostname yes\n  HostName real2.example.com\n\
             Host real2.example.com\n  User r2\n\
             Host cn\n  CanonicalizeHostname yes\n  CanonicalDomains example.com\n\
             Host pq\n  ProxyCommand \"none\"\n  ProxyJump j\n\
             Host pk\n  ProxyCommand none # c\n\
             Host pe\n  ProxyCommand \"\"\n\
             Host pn\n  ProxyCommand =NoNe\n  ProxyJump j3\n\
             Host ua\n  HostName ÄB.De\n\
             Host td\n  CanonicalizeHostname yes\n  HostName localhost.\n",
        )
        .unwrap();
        let result = import_ssh_config(&proxy).unwrap();
        let dns = "ssh may rename its host through DNS (`CanonicalizeHostname` with `CanonicalDomains` or `CanonicalizePermittedCNAMEs`, or a host name ending in `.`) and read the file again for the new name, which this import cannot know; to import it, import a copy of the file that holds only a `Host` block for it, with its full host name in `HostName` and the `Port`, `User` and `IdentityFile` ssh uses for it, and no `CanonicalizeHostname`, `CanonicalDomains` or `CanonicalizePermittedCNAMEs` line";
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(
            skipped,
            [
                (
                    "pc",
                    "ssh reaches it through a `ProxyCommand`, which AeroFTP does not run"
                ),
                ("pj", JUMP),
                ("cn", dns),
                (
                    "pq",
                    "ssh reaches it through a `ProxyCommand`, which AeroFTP does not run"
                ),
                (
                    "pk",
                    "ssh reaches it through a `ProxyCommand`, which AeroFTP does not run"
                ),
                (
                    "pe",
                    "ssh reaches it through a `ProxyCommand`, which AeroFTP does not run"
                ),
                ("td", dns),
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
            ("cy", "real2.example.com", 22, "r2", "", ""),
            ("real2.example.com", "real2.example.com", 22, "r2", "", ""),
            // `none` is the word alone, any case; ssh lowercases the host in
            // ASCII only.
            ("pn", "pn", 22, me, "", ""),
            ("ua", "Äb.de", 22, me, "", ""),
        ];
        assert_eq!(proxy_got, proxy_want);

        // `Match` conditions the file decides, and the second reading a
        // `Match final` asks for, which keeps the host name of the first
        // (`plain` and `none` stay themselves); `ProxyCommand none` and
        // `ProxyJump none`.
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
             Host ub\n  HostName ÄB.De\n\
             Match host äb.de\n  User wrong\n\
             Match host Äb.DE\n  User ub\n\
             Match host nomatch all\n  User never\n\
             Host jumped\n\
             Match originalhost plain,none,jumped\n  ProxyJump j2\n\
             Match final\n  HostName prod.example.com\n",
        )
        .unwrap();
        let result = import_ssh_config(&decided).unwrap();
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(skipped, [("jumped", JUMP)]);
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
            ("web", "web.example.com", 2201, "deploy", "/k/deploy", ""),
            ("gitlab", "gitlab.example.com", 2202, "git", "", ""),
            ("real", "real.example.com", 2202, "r", "", ""),
            ("real.example.com", "real.example.com", 2202, "r", "", ""),
            ("plain", "plain", 2202, me, "", ""),
            ("none", "none", 2202, me, "", ""),
            ("up", "up.example.com", 2202, "upcase", "", ""),
            // Only ASCII letters are compared in any case.
            ("ub", "Äb.de", 2202, "ub", "", ""),
        ];
        assert_eq!(decided_got, decided_want);

        // A condition known only when ssh runs: a host is skipped when such
        // a `Match` may set a value it has none for yet, and only then.
        let unevaluated = dir.path().join("unevaluated");
        std::fs::write(
            &unevaluated,
            "Host lab\n  HostName lab.example\n  Port 2201\n\
             Match originalhost lab* exec \"true\"\n  CanonicalizeHostname yes\n\
             Host lab.example\n  Port 2222\n\
             Host e\n  HostName e.example.com\n  User eown\n  Port 2202\n\
             Host f\n  HostName f.example.com\n  Port 2204\n\
             Match host f.example.com exec \"false\"\n  User mf\n\
             Match host nomatch exec \"x\"\n  User never\n\
             Match exec \"true\"\n  Port 2500\n\
             Host g\n  User gu\n\
             Match tagged \"\"\n  Compression yes\n\
             Match command=\n  Compression yes\n",
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
            format!("the `Match` block on line {line} of the file may set its `{key}`, on a condition known only when ssh runs; give the host its `{key}` in a `Host` block above that line and import again")
        };
        assert_eq!(
            skipped,
            [
                // It may make ssh read the file again, for `lab.example`.
                ("lab", reason(4, "canonicalizehostname").as_str()),
                ("lab.example", reason(4, "canonicalizehostname").as_str()),
                ("f", reason(15, "user").as_str()),
                ("g", reason(19, "port").as_str())
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

    /// What `ssh -G` (OpenSSH 10.2p1) reads for these hosts: a numeric host
    /// name in its canonical form for the second reading, `true` as `yes`,
    /// a permitted CNAME as a rename, `?` as one byte, and `ProxyJump none
    /// # c` as a hop called `none`; a host reached through a jump is skipped,
    /// and the reports repeat no value of the file.
    #[test]
    fn a_host_name_and_its_patterns_read_as_ssh_reads_them() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "Host v6\n  HostName 2001:db8:0:0:0:0:0:1\n\
             Host v6case\n  HostName 2001:DB8::2\n\
             Host v4\n  HostName 127.1\n\
             Host v4hex\n  HostName 0x7f.0.0.2\n\
             Host first\n  HostName 2001:db8:0:0:0:0:0:3\n\
             Match host 2001:db8::3\n  User second-reading\n\
             Host t\n  CanonicalizeHostname true\n  HostName real.example.com\n\
             Host cnames\n  CanonicalizeHostname yes\n  CanonicalizePermittedCNAMEs *:*\n\
             Host cnone\n  CanonicalizeHostname yes\n  CanonicalizePermittedCNAMEs none\n  HostName cnone.example.com\n\
             Host coff\n  CanonicalizePermittedCNAMEs *:*\n  HostName coff.example.com\n\
             Host \u{e9}\n\
             Host pjc\n  ProxyJump none # bastion\n\
             Host pjn\n  ProxyJump NoNe\n\
             Host uri\n  ProxyJump ssh://bob;fp=SECRET@jump.example.com:2200,ssh://j2\n\
             Host PJ*\n  Port 2298\n\
             Host pj*\n  User lower\n\
             Match final\n\
             Host 2001:db8::1 127.0.0.1 real.example.com\n  Port 2201\n\
             Host 2001:DB8::2\n  Port 2202\n\
             Host 2001:db8::2\n  Port 2299\n\
             Host 127.0.0.2\n  Port 2203\n\
             Host ?\n  HostName one-byte.example.com\n",
        )
        .unwrap();
        let result = import_ssh_config(&config).unwrap();
        let me = local_account().0.unwrap_or_default();
        let me = me.as_str();
        let dns = "ssh may rename its host through DNS (`CanonicalizeHostname` with `CanonicalDomains` or `CanonicalizePermittedCNAMEs`, or a host name ending in `.`) and read the file again for the new name, which this import cannot know; to import it, import a copy of the file that holds only a `Host` block for it, with its full host name in `HostName` and the `Port`, `User` and `IdentityFile` ssh uses for it, and no `CanonicalizeHostname`, `CanonicalDomains` or `CanonicalizePermittedCNAMEs` line";
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .filter(|s| !s.name.contains(['?', '*', ':', '.']))
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
        assert_eq!(skipped, [("cnames", dns), ("pjc", JUMP), ("uri", JUMP),]);
        let report = serde_json::to_string(&result).unwrap();
        assert!(!report.contains("SECRET"), "{report}");
        let got: Vec<(&str, &str, u32, &str, &str)> = result
            .servers
            .iter()
            .filter(|s| !s.name.contains([':', '.']) || s.name == "\u{e9}")
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
                    jump,
                )
            })
            .collect();
        assert_eq!(
            got,
            [
                ("v6", "2001:db8::1", 2201, me, ""),
                ("v6case", "2001:DB8::2", 2202, me, ""),
                ("v4", "127.0.0.1", 2201, me, ""),
                ("v4hex", "127.0.0.2", 2203, me, ""),
                ("first", "2001:db8::3", 22, "second-reading", ""),
                ("t", "real.example.com", 2201, me, ""),
                ("cnone", "cnone.example.com", 22, me, ""),
                ("coff", "coff.example.com", 22, me, ""),
                ("\u{e9}", "\u{e9}", 22, me, ""),
                // `Host` patterns compare case: `PJ*` does not apply to `pjn`.
                ("pjn", "pjn", 22, "lower", ""),
            ]
        );

        // `true`, in any case, reads the file again as `yes` does, with no
        // `Match final` to ask for it (`ssh -G`: port 2201).
        let truthy = dir.path().join("truthy");
        std::fs::write(
            &truthy,
            "Host t\n  CanonicalizeHostname TRUE\n  HostName real.example.com\n\
             Host real.example.com\n  Port 2201\n",
        )
        .unwrap();
        let result = import_ssh_config(&truthy).unwrap();
        assert_eq!(result.servers[0].name, "t");
        assert_eq!(result.servers[0].port, 2201);

        // A file whose hosts times `Match` and wildcard blocks pass the
        // budget is refused whole, with the reason, instead of resolved for
        // as long as it takes; one block fewer is resolved.
        for (rules, refused) in [(1000, true), (999, false)] {
            let mut work = String::new();
            for i in 0..rules {
                work.push_str(&format!("Match originalhost o{i}\n  Port 2222\n"));
            }
            for i in 0..2001 {
                work.push_str(&format!("Host h{i}\n"));
            }
            let file = dir.path().join("work");
            std::fs::write(&file, work).unwrap();
            let result = import_ssh_config(&file).unwrap();
            let reason = format!("the file reads each of its 2001 hosts against {rules} `Match` and wildcard `Host` blocks, more than one import resolves; {SMALLER_FILE}");
            assert_eq!(
                result.servers.len(),
                if refused { 0 } else { 2001 },
                "{:?}",
                result.skipped.first().map(|s| &s.reason)
            );
            assert_eq!(
                result.skipped.iter().filter(|s| s.reason == reason).count(),
                if refused { 2001 } else { 0 }
            );
        }

        // A file over the cap is refused whole, with the reason, rather than
        // read in part: a rule past the cap may apply to an earlier host.
        let mut big = String::from("Host early\n");
        for i in 0..MAX_HOSTS {
            big.push_str(&format!("Host h{i}\n"));
        }
        big.push_str("Host early\n  Port 2222\n");
        let capped = dir.path().join("capped");
        std::fs::write(&capped, big).unwrap();
        let result = import_ssh_config(&capped).unwrap();
        assert!(result.servers.is_empty());
        assert_eq!(result.skipped.len(), MAX_HOSTS);
        assert!(result.skipped.iter().all(|s| s.reason == format!("the file has more than {MAX_HOSTS} `Host` and `Match` blocks, more than one import reads; {SMALLER_FILE}")));
    }
    /// `%u`, `%d` and a `%r` with no `User` are the local account's, read
    /// from its passwd entry as ssh does, whatever `USER` and `HOME` say;
    /// so is the folder a leading `~/` stands for, and the user name of a host
    /// with no `User`.
    #[cfg(unix)]
    #[test]
    fn the_local_tokens_are_the_accounts_not_the_environment() {
        if !in_child_with_env(
            "the_local_tokens_are_the_accounts_not_the_environment",
            &[
                ("HOME", "/tmp/aeroftp-not-the-home"),
                ("USER", "not-the-user"),
                ("LOGNAME", "~"),
            ],
        ) {
            return;
        }
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
        // `HOME` and `USER` are set to other values for this process alone.
        assert_eq!(std::env::var("USER").as_deref(), Ok("not-the-user"));
        let dir_path = tempfile::tempdir().unwrap();
        let source = dir_path.path().join("config");
        std::fs::write(
            &source,
            "Host a\n  HostName h.example.com\n  IdentityFile %d/id_%u_%r\n\nHost t\n  HostName h.example.com\n  IdentityFile ~/k\n\nHost e\n  HostName h.example.com\n  User ${USER}\n  IdentityFile ${HOME}/k\n\nHost tl\n  HostName h.example.com\n  IdentityFile ${LOGNAME}/k\n",
        )
        .unwrap();
        let result = import_ssh_config(&source).unwrap();
        let reasons: Vec<_> = result.skipped.iter().map(|s| &s.reason).collect();
        assert_eq!(result.servers.len(), 3, "{reasons:?}");
        // A `~` a variable gives is not a home folder to ssh: `ssh -vvv`
        // loads `~/k`, a folder called `~`, and the host is skipped.
        assert_eq!(
            reasons,
            ["its key file path starts with `~` only once its tokens or variables are expanded, which ssh reads as a folder called `~`, not a home folder; write the path out and import again"]
        );
        if std::process::Command::new("ssh").arg("-V").output().is_ok() {
            assert_eq!(ssh_first_key(&source, "tl").as_deref(), Some("~/k"));
        }
        let a = &result.servers[0];
        // With no `User`, the account's name, as ssh logs in, not `$USER`.
        assert_eq!(a.username, name);
        assert_eq!(
            a.options.as_ref().unwrap()["private_key_path"],
            format!("{dir}/id_{name}_{name}")
        );
        assert_eq!(
            result.servers[1].options.as_ref().unwrap()["private_key_path"],
            format!("{dir}/k")
        );
        // `${USER}` and `${HOME}` are the environment's, as ssh reads them:
        // only the tokens above come from the account.
        let e = &result.servers[2];
        assert_eq!(e.username, "not-the-user");
        assert_eq!(
            e.options.as_ref().unwrap()["private_key_path"],
            "/tmp/aeroftp-not-the-home/k"
        );
        // A user name that cannot be read is refused, not taken as empty.
        let unknown = SshTokens {
            alias: "a",
            host: "a",
            user: None,
            port: 22,
            account: &(None, None),
            steps: &Steps::default(),
        };
        assert_eq!(
            ssh_expand("key file path", "/k/id_%r", "dhnpru", &unknown, true),
            Err(
                "its key file path uses the ssh token %r, which AeroFTP cannot resolve".to_string()
            )
        );
    }

    /// A `Match` on a condition known only when ssh runs counts its keys:
    /// if it may take the host past the 100 `IdentityFile` lines ssh reads,
    /// ssh may refuse to connect, and the host is skipped; one that cannot
    /// leaves it imported, with the key it tries first.
    #[test]
    fn a_match_known_only_when_ssh_runs_counts_its_keys() {
        let dir = tempfile::TempDir::new().unwrap();
        let keys: String = (0..99)
            .map(|i| format!("  IdentityFile /k/{i}\n"))
            .collect();
        let with = |extra: &str| {
            import_text(
                dir.path(),
                &format!("Host lab\n  HostName l.example.com\n{keys}Match exec true\n{extra}"),
            )
        };
        let crossing = with("  IdentityFile /k/a\n  IdentityFile /k/b\n");
        assert!(crossing.servers.is_empty());
        assert_eq!(crossing.skipped[0].reason, format!("the `Match` block on line 102 of the file may add `IdentityFile` lines past the {MAX_IDENTITY_FILES} ssh reads, on a condition known only when ssh runs, and ssh then refuses to connect; give the host fewer `IdentityFile` lines and import again"));
        // Keys it shares with the host are no new ones.
        for extra in [
            "  IdentityFile /k/a\n",
            "  IdentityFile /k/0\n  IdentityFile /k/a\n",
        ] {
            let result = with(extra);
            let lab = result.servers.iter().find(|s| s.name == "lab");
            assert_eq!(
                lab.map(|s| s.options.as_ref().unwrap()["private_key_path"].clone()),
                Some(serde_json::json!("/k/0")),
                "{extra:?}: {:?}",
                reasons(&result)
            );
        }
        // A later block that applies counts what that `Match` may add.
        let later = with("  IdentityFile /k/a\nHost lab\n  IdentityFile /k/b\n");
        assert!(later.servers.is_empty(), "{:?}", reasons(&later));
        assert!(later.skipped[0]
            .reason
            .starts_with("the `Match` block on line 102"));
    }

    /// The services file Windows reads is parsed as the system reads it: a
    /// name or one of its aliases, for `tcp`, up to a comment.
    #[test]
    fn a_service_name_is_read_from_the_services_file() {
        let text = "ssh 22/tcp\nlab 2200/tcp labsvc # lab #x 1/tcp\nlab 99/udp\nudponly 98/udp\n";
        assert_eq!(service_in(text, "ssh"), Some(22));
        assert_eq!(service_in(text, "labsvc"), Some(2200));
        assert_eq!(service_in(text, "lab"), Some(2200));
        assert_eq!(service_in(text, "#x"), None);
        assert_eq!(service_in(text, "udponly"), None);
        assert_eq!(service_in(text, "SSH"), None);
    }

    /// Runs the test `name` of this module again in a child process with
    /// `env` set there (`Command::env`), so no variable changes in this
    /// process, where other tests read the environment at the same time.
    /// `true` in the child, where the test body runs; in this process,
    /// `false` once the child passed.
    fn in_child_with_env(name: &str, env: &[(&str, &str)]) -> bool {
        const CHILD: &str = "AEROFTP_SSH_IMPORT_TEST_CHILD";
        if std::env::var(CHILD).as_deref() == Ok(name) {
            return true;
        }
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                &format!("ssh_config_import::tests::{name}"),
                "--exact",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD, name);
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && stdout.contains("1 passed"),
            "{stdout}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        false
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
        let secret = "s3cr3t-value-never-imported";
        // The variable is set in a child process alone, the one the test
        // body runs in.
        if !in_child_with_env(
            "an_imported_token_is_expanded_as_ssh_reads_it",
            &[("AEROFTP_SSH_TEST_SECRET", secret)],
        ) {
            return;
        }
        let home = local_account().1.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        std::fs::write(
            &source,
            "Host al\n  HostName %h.example.com\n  Port 2200\n  User %n-x\n  IdentityFile %d/id_%h_%r_%%\n\nHost tok\n  HostName h.example.com\n  IdentityFile /keys/%C\n\nHost secret\n  HostName h.example.com\n  User ${AEROFTP_SSH_TEST_SECRET}\n\nHost open\n  HostName h.example.com\n  IdentityFile /keys/${HOME\n",
        )
        .unwrap();
        let result = import_ssh_config(&source).unwrap();
        let skipped: Vec<(&str, &str)> = result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect();
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
            assert!(
                out.status.success(),
                "ssh -G {}: {}",
                config.display(),
                String::from_utf8_lossy(&out.stderr)
            );
            let lines = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| {
                    l.starts_with("hostname ")
                        || l.starts_with("port ")
                        || (user_tokens && l.starts_with("user "))
                })
                .map(str::to_string)
                .collect::<Vec<_>>();
            for key in ["hostname ", "port "] {
                assert!(lines.iter().any(|l| l.starts_with(key)), "{key}{lines:?}");
            }
            lines
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
            "Host esc\n  HostName h.example.com # prod\n  User 'o\\'brien'\n  IdentityFile \"C:\\\\Keys\\\\my id\"\n\n\
             Host eq\n  HostName=e.example.com\n  User = ops\n\n\
             Host m\n  HostName m.example.com\nMatch all\n  User matched\n",
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
        assert_eq!(
            (by("eq").host.as_str(), by("eq").username.as_str()),
            ("e.example.com", "ops")
        );
        assert_eq!(by("m").username, "matched");
        // `ProxyJump` is read up to its comment, a `#` even inside a word,
        // and a quote inside the comment is not read; a host it applies to
        // is skipped, `none` alone aside.
        let jumps = "Host esc\n  ProxyJump j1,j2 # two hops\n\
                     Host pj\n  ProxyJump jump.example.com#x\n\
                     Host pq\n  ProxyJump j1 # \"x\n\
                     Host pc\n  ProxyJump none # c\n";
        let config = SshConfig::new(parse_ssh_config(jumps).0);
        let jump = |name: &str| host_values(&config, name).unwrap()["proxyjump"].clone();
        assert_eq!(jump("esc"), "j1,j2");
        assert_eq!(jump("pj"), "jump.example.com");
        assert_eq!(jump("pq"), "j1");
        assert_eq!(jump("pc"), "none # c");
        let jumps_file = dir.path().join("jumps");
        std::fs::write(&jumps_file, jumps).unwrap();
        let result = import_ssh_config(&jumps_file).unwrap();
        assert!(result.servers.is_empty());
        let reasons: Vec<&str> = result.skipped.iter().map(|s| s.reason.as_str()).collect();
        assert_eq!(reasons, [JUMP; 4]);

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
                "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows",
            ),
            (
                "Host fine\n  HostName f.example.com\n  Port 65536\n",
                3,
                "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows",
            ),
            (
                "Host fine\n  HostName f.example.com\n  Port 99999999999999999999\n",
                3,
                "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch\n  Port 1\n",
                3,
                "gives `Match` no condition",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch bogus x\n  Port 1\n",
                3,
                "gives `Match` a condition ssh does not know",
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
            (
                "Host fine\n  HostName f.example.com\nMatch final all host x\n  Port 1\n",
                3,
                "combines `Match all` with another condition",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch tagged\n  Port 1\n",
                3,
                "gives the `Match` condition `tagged` no value",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch final=x\n  Port 1\n",
                3,
                "gives the `Match` condition `final` a value",
            ),
            (
                "Host fine\n  HostName f.example.com\nMatch host a all=\n  Port 1\n",
                3,
                "gives the `Match` condition `all` a value",
            ),
            (
                "Host fine\n  HostName f.example.com\n  ProxyCommand\n",
                3,
                "gives `proxycommand` no value",
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
        // a host's block or before the first `Host`; a name the system does
        // not know (`22x`, `Ssh`) refuses the file ([`REFUSED_BY_10`]).
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
        // ssh 10.2 is the reference: from 10.0 ssh refuses every one of these
        // files. An earlier ssh, or the Windows port, reads some of them, and
        // refusing a file that ssh reads is the safe direction: the hosts are
        // listed as skipped with the reason, never imported as something
        // else. Those files are listed here, so a new difference or one that
        // is gone turns this red. Measured in CI: OpenSSH 8.9 (Ubuntu 22.04)
        // and the Windows port read the `all=` file.
        const READ_BEFORE_10: [&str; 1] =
            ["Host fine\n  HostName f.example.com\nMatch host a all=\n  Port 1\n"];
        let version = std::process::Command::new("ssh")
            .arg("-V")
            .output()
            .map(|out| String::from_utf8_lossy(&out.stderr).trim().to_string())
            .unwrap_or_default();
        let mut read_here = Vec::new();
        for (config, _, _) in refused {
            std::fs::write(&bad, config).unwrap();
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&bad)
                .arg("fine")
                .output()
                .unwrap();
            if out.status.success() {
                read_here.push(config);
            }
        }
        let expected: Vec<&str> = if ssh_major().is_some_and(|major| major >= 10) {
            Vec::new()
        } else {
            READ_BEFORE_10.to_vec()
        };
        let extra: Vec<&str> = read_here
            .iter()
            .copied()
            .filter(|c| !expected.contains(c))
            .collect();
        let missing: Vec<&str> = expected
            .iter()
            .copied()
            .filter(|c| !read_here.contains(c))
            .collect();
        assert!(
            extra.is_empty() && missing.is_empty(),
            "{version} reads files this import refuses: not in the list {extra:?}, in the list but refused {missing:?}"
        );
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
        // The jumps the import reads are the ones ssh reads.
        for (alias, jump) in [
            ("esc", "j1,j2"),
            ("pj", "jump.example.com"),
            ("pq", "j1"),
            ("pc", "none"),
        ] {
            let out = std::process::Command::new("ssh")
                .arg("-G")
                .arg("-F")
                .arg(&jumps_file)
                .arg(alias)
                .output()
                .unwrap();
            let got = String::from_utf8_lossy(&out.stdout).to_string();
            assert!(
                got.lines().any(|l| l == format!("proxyjump {jump}")),
                "{alias}: {got}"
            );
        }
    }

    /// The lines ssh 10.2p1 refuses the whole file for, in a block that
    /// applies to no host to import, with the reason the import gives
    /// (`ssh -G` on 10.2p1; older clients read some of them differently, so
    /// the comparison with the local ssh runs only on 10 or later).
    const REFUSED_BY_10: [(&str, &str); 34] = [
        ("Match host \"#lab\"", "gives the `Match` condition `host` no value"),
        ("Match host=#lab", "gives the `Match` condition `host` no value"),
        ("Match user \"#x\"", "gives the `Match` condition `user` no value"),
        ("Match tagged \"#x\"", "gives the `Match` condition `tagged` no value"),
        ("Host o\n  Port \"2200 \"", "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows"),
        ("Host o\n  ProxyJump ssh://u%%2B@j.example.com", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ssh://j%2Eexample.com", "gives `proxyjump` a jump host ssh cannot read"),
        ("Match final originalhost fine all", "combines `Match all` with another condition"),
        ("Match canonical final all", "combines `Match all` with another condition"),
        ("Match all \"\" x", "combines `Match all` with another condition"),
        ("Match \"#x\"", "gives `Match` no condition"),
        ("Match host fine \"\"", "gives `Match` a condition ssh does not know"),
        ("Host o\n  ProxyJump bastion:0", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump bastion:x", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump a:65536", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump @a", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump a@", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump a/b", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ::1", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump [::1", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump j1,,j2", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ,a", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ssh://u@a/x", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ssh://[::1]:22", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ssh://a;x", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  ProxyJump ssh://a:/x", "gives `proxyjump` a jump host ssh cannot read"),
        ("Host o\n  CanonicalizeHostname maybe", "gives `canonicalizehostname` a value ssh does not take"),
        ("Host o\n  CanonicalizeHostname yes no", "gives `canonicalizehostname` more than one value"),
        ("Host o\n  CanonicalDomains none a.b", "gives `canonicaldomains` `none` with other values"),
        ("Host o\n  CanonicalDomains _a.b -x", "gives `canonicaldomains` a domain name ssh refuses"),
        ("Host o\n  CanonicalDomains a..b", "gives `canonicaldomains` a domain name ssh refuses"),
        ("Host o\n  CanonicalizePermittedCNAMEs a: b:c", "gives `canonicalizepermittedcnames` a rule ssh cannot read"),
        ("Host o\n  Port 22x", "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows"),
        ("Host o\n  Port Ssh", "gives `port` a value that is not a port from 1 to 65535 nor a service name the system knows"),
    ];

    /// The same lines that ssh 10.2p1 reads: the file imports, and the host
    /// `fine` gets the port, the user and the first key ssh uses for it.
    const READ_BY_10: [&str; 29] = [
        "  Port \" 2200\"",
        "  Port \"\t+2200\"",
        "  User \"*buildsvc\"\nMatch user *svc\n  Port 2200",
        "  User \"?x\"\nMatch user ?x\n  Port 2200",
        "  IdentityFile ~root/k",
        "  IdentityFile ~root",
        "  IdentityFile ~//k",
        "  IdentityFile ~/k",
        "Host o\n  ProxyJump ssh://u%2B@j.example.com",
        "Match host o all\n  Port 1",
        "Match !final all\n  Port 1",
        "Match all \"\"\n  Port 1",
        "Match all \"#\" x\n  Port 1",
        "Match host o \"#b\" bogus\n  Port 1",
        "Host o\n  ProxyJump a:",
        "Host o\n  ProxyJump a@b@c",
        "Host o\n  ProxyJump [::1]:22",
        "Host o\n  ProxyJump ssh://u@a:22/",
        "Host o\n  ProxyJump ssh://bob;fp=X@jump:2200",
        "Host o\n  ProxyJump a:+2,b:02",
        "Host o\n  ProxyJump u@a:22,v@b",
        "Host o\n  ProxyJump a;b",
        "Host o\n  ProxyJump none,a",
        "Host o\n  ProxyJump a:ssh",
        "Host o\n  CanonicalizeHostname YES",
        "Host o\n  CanonicalDomains a.b.",
        "Host o\n  CanonicalizePermittedCNAMEs :b *",
        "Host o\n  CanonicalizePermittedCNAMEs NONE",
        "Host o\n  IdentityFile NONE",
    ];

    /// The values `ssh -G` reads for `host` in `file`, by keyword.
    /// The command ssh runs for the jump host of `host` in `file` (`ssh
    /// -vvv`: "Executing proxy command: exec ssh ..."), with the `ssh` it
    /// names a stand-in that exits at once, so no jump is ever opened (the
    /// command does not pass on `BatchMode` or `ConnectTimeout`). `None` where
    /// the stand-in cannot be put in its place.
    fn ssh_jump_command(file: &Path, host: &str) -> Option<String> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            use std::os::unix::process::CommandExt;
            let path = std::env::var_os("PATH").unwrap_or_default();
            let real = std::env::split_paths(&path)
                .map(|dir| dir.join("ssh"))
                .find(|ssh| ssh.is_file())?;
            let stand_in = tempfile::tempdir().unwrap();
            let ran = stand_in.path().join("ran");
            let script = stand_in.path().join("ssh");
            std::fs::write(
                &script,
                format!("#!/bin/sh\n: > '{}'\nexit 1\n", ran.display()),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let mut dirs = vec![stand_in.path().to_path_buf()];
            dirs.extend(std::env::split_paths(&path));
            // A test that forks while the script is still open for writing
            // makes its exec fail for a moment (ETXTBSY): run again then.
            let mut log = String::new();
            for _ in 0..5 {
                let out = std::process::Command::new(&real)
                    .arg0("ssh")
                    .env("PATH", std::env::join_paths(&dirs).unwrap())
                    .args(["-vvv", "-F"])
                    .arg(file)
                    .args([
                        "-o",
                        "BatchMode=yes",
                        "-o",
                        "ConnectTimeout=5",
                        host,
                        "true",
                    ])
                    .output()
                    .unwrap();
                log = String::from_utf8_lossy(&out.stderr).into_owned();
                let command = log.lines().find_map(|l| {
                    l.split_once("Executing proxy command: ")
                        .map(|(_, c)| c.to_string())
                });
                if ran.exists() {
                    return Some(command.unwrap_or_else(|| panic!("{host}: {log}")));
                }
            }
            panic!("{host}: the stand-in for ssh never ran: {log}");
        }
        #[cfg(not(unix))]
        {
            let _ = (file, host);
            None
        }
    }

    fn ssh_g(file: &Path, host: &str) -> HashMap<String, String> {
        let out = std::process::Command::new("ssh")
            .args(["-G", "-F"])
            .arg(file)
            .arg(host)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "ssh -G {host}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The first key file ssh loads for `host` in `file` (`ssh -vvv`:
    /// "identity file <path>"), against a local listener that answers with
    /// a version line and closes; `None` when ssh stops before it loads one.
    fn ssh_first_key(file: &Path, host: &str) -> Option<String> {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                if let Ok((mut stream, _)) = listener.accept() {
                    let _ = stream.write_all(b"SSH-2.0-probe\r\n");
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
        let out = std::process::Command::new("ssh")
            .args(["-vvv", "-F"])
            .arg(file)
            .args(["-o", "HostName=127.0.0.1", "-o", &format!("Port={port}")])
            .args(["-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=no"])
            .args([
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "ConnectTimeout=5",
            ])
            .arg(host)
            .arg("true")
            .output()
            .unwrap();
        drop(server);
        String::from_utf8_lossy(&out.stderr).lines().find_map(|l| {
            l.strip_prefix("debug1: identity file ")
                .and_then(|rest| rest.rsplit_once(" type "))
                .map(|(path, _)| path.to_string())
        })
    }

    fn ssh_major() -> Option<u32> {
        let out = std::process::Command::new("ssh").arg("-V").output().ok()?;
        let text = String::from_utf8_lossy(&out.stderr).to_string();
        text.strip_prefix("OpenSSH_")?
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse()
            .ok()
    }

    /// Every line ssh refuses a file for refuses it here, in a block that
    /// applies to no host and with no word of the line in the reason, and
    /// every line ssh reads is read; the local ssh, when 10 or later, agrees
    /// on each.
    #[test]
    fn a_file_ssh_refuses_is_refused_and_one_it_reads_is_read() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("config");
        let ssh = ssh_major().is_some_and(|major| major >= 10);
        if !ssh {
            eprintln!(
                "SKIPPED: no ssh 10 or later here, so the files are not compared with ssh -G"
            );
        }
        let ssh_reads = |text: &str| {
            std::fs::write(&file, text).unwrap();
            std::process::Command::new("ssh")
                .args(["-G", "-F"])
                .arg(&file)
                .arg("fine")
                .output()
                .unwrap()
                .status
                .success()
        };
        for (line, reason) in REFUSED_BY_10 {
            let text = format!("Host fine\n  HostName f.example.com\n{line}\n");
            std::fs::write(&file, &text).unwrap();
            let result = import_ssh_config(&file).unwrap();
            assert!(result.servers.is_empty(), "{line:?}");
            let line_no = 2 + line.lines().count();
            for skip in &result.skipped {
                assert_eq!(
                    skip.reason,
                    format!("line {line_no} of the file {reason}, so ssh refuses the whole file and reads none of its hosts"),
                    "{line:?}"
                );
            }
            if ssh {
                assert!(!ssh_reads(&text), "ssh read {line:?}");
            }
        }
        for line in READ_BY_10 {
            let text = format!("Host fine\n  HostName f.example.com\n{line}\n");
            std::fs::write(&file, &text).unwrap();
            let result = import_ssh_config(&file).unwrap();
            // Only a Unix system has a passwd entry to read `~root` from
            // ([`passwd_home`]): elsewhere the host is skipped, as ssh stops.
            if cfg!(not(unix)) && line.contains("~root") {
                assert!(result.servers.is_empty(), "{line:?}");
                assert!(
                    result
                        .skipped
                        .iter()
                        .any(|s| s.reason.contains("an account this system does not have")),
                    "{line:?}: {:?}",
                    result.skipped.iter().map(|s| &s.reason).collect::<Vec<_>>()
                );
                continue;
            }
            let fine = result.servers.iter().find(|s| s.name == "fine");
            let Some(fine) = fine else {
                panic!(
                    "{line:?}: {:?}",
                    result.skipped.iter().map(|s| &s.reason).collect::<Vec<_>>()
                );
            };
            if ssh {
                assert!(ssh_reads(&text), "ssh refused {line:?}");
                let read = ssh_g(&file, "fine");
                assert_eq!(fine.port.to_string(), read["port"], "{line:?}");
                assert_eq!(fine.username, read["user"], "{line:?}");
                // `ssh -G` prints a key path with its `~` as written: the
                // key compared is the one ssh loads.
                if line.starts_with("  IdentityFile") {
                    let key = fine.options.as_ref().unwrap()["private_key_path"].clone();
                    assert_eq!(
                        Some(key.as_str().unwrap().to_string()),
                        ssh_first_key(&file, "fine"),
                        "{line:?}"
                    );
                }
            }
        }
        // A `~` naming an account the system does not have stops ssh, and
        // skips the host.
        let text = "Host fine\n  HostName f.example.com\n  IdentityFile ~no-such-account-c4f1/k\n";
        std::fs::write(&file, text).unwrap();
        let result = import_ssh_config(&file).unwrap();
        assert!(result.servers.is_empty());
        assert_eq!(result.skipped[0].reason, "its key file path starts with `~` and the name of an account this system does not have, and ssh stops there");
        if ssh {
            assert_eq!(ssh_first_key(&file, "fine"), None);
        }
        // The word in a condition's place may hold anything: it is not in
        // the report.
        for (text, reason) in [
            (
                "Host fine\nMatch Sentinel-c4f1 x\n  Port 1\n",
                "gives `Match` a condition ssh does not know",
            ),
            (
                "Host fine\n  Port Sentinel-c4f1\n",
                "gives `port` a value that is not a port",
            ),
            (
                "Host fine\n  ProxyJump Sentinel-c4f1:x\n",
                "gives `proxyjump` a jump host ssh cannot read",
            ),
            (
                "Host fine\n  CanonicalizeHostname Sentinel-c4f1\n",
                "gives `canonicalizehostname` a value",
            ),
        ] {
            std::fs::write(&file, text).unwrap();
            let report = serde_json::to_string(&import_ssh_config(&file).unwrap()).unwrap();
            assert!(!report.to_lowercase().contains("sentinel-c4f1"), "{report}");
            assert!(report.contains(reason), "{report}");
        }
    }

    fn import_text(dir: &Path, text: &str) -> SshImportResult {
        let file = dir.join("config");
        std::fs::write(&file, text).unwrap();
        import_ssh_config(&file).unwrap()
    }

    fn reasons(result: &SshImportResult) -> Vec<(&str, &str)> {
        result
            .skipped
            .iter()
            .map(|s| (s.name.as_str(), s.reason.as_str()))
            .collect()
    }

    /// `IdentityFile none` is a line ssh counts and a key it does not try:
    /// the profile keeps the first other key, from any block, and a host
    /// with more than 100 lines (repeats and `none` included, both readings
    /// counted) is skipped, as ssh refuses to connect to it.
    #[test]
    fn identity_files_read_as_ssh_loads_them() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = |result: &SshImportResult, name: &str| {
            result.servers.iter().find(|s| s.name == name).map(|s| {
                s.options
                    .as_ref()
                    .and_then(|o| o["private_key_path"].as_str())
                    .unwrap_or("")
                    .to_string()
            })
        };
        let result = import_text(
            dir.path(),
            "IdentityFile none\nHost lab\n  IdentityFile /keys/lab\nHost in\n  IdentityFile NONE\n  IdentityFile /keys/in\nHost bare\n  IdentityFile none\n",
        );
        assert_eq!(key(&result, "lab").as_deref(), Some("/keys/lab"));
        assert_eq!(key(&result, "in").as_deref(), Some("/keys/in"));
        assert_eq!(key(&result, "bare").as_deref(), Some(""));
        let lines = |n: usize, distinct: usize| -> String {
            (0..n)
                .map(|i| format!("  IdentityFile /k/{}\n", i % distinct))
                .collect()
        };
        let cases = [
            (format!("Host fine\n{}", lines(101, 101)), false),
            (
                format!("Host fine\n{}  IdentityFile /k/5\n", lines(100, 100)),
                false,
            ),
            (
                format!(
                    "Host fine\n{}  IdentityFile none\n  IdentityFile none\n",
                    lines(99, 99)
                ),
                false,
            ),
            (
                format!("IdentityFile /k/a\nHost fine\n{}", lines(100, 100)),
                false,
            ),
            (
                format!("Host fine\n{}Match final\n", lines(100, 100)),
                false,
            ),
            (
                format!(
                    "Host fine\n{}Match final\n  IdentityFile /k/x\n  IdentityFile /k/y\n",
                    lines(99, 99)
                ),
                false,
            ),
            (format!("Host fine\n{}Match final\n", lines(60, 60)), true),
            (format!("Host fine\n{}", lines(150, 50)), true),
            (format!("Host fine\n{}", lines(100, 100)), true),
        ];
        let ssh = ssh_major().is_some_and(|major| major >= 10);
        for (text, imports) in &cases {
            let result = import_text(dir.path(), text);
            assert_eq!(
                result.servers.len(),
                usize::from(*imports),
                "{}",
                text.len()
            );
            if !imports {
                assert_eq!(
                    reasons(&result),
                    [("fine", "more than 100 `IdentityFile` lines apply to it, and ssh refuses to connect with more")]
                );
            }
            if ssh {
                let out = std::process::Command::new("ssh")
                    .args(["-G", "-F"])
                    .arg(dir.path().join("config"))
                    .arg("fine")
                    .output()
                    .unwrap();
                assert_eq!(out.status.success(), *imports, "{}", text.len());
            }
        }
    }

    /// The host a `Host` line is imported as is one it applies to: `Host
    /// !old old backup` is `backup`, and a line that excludes every host it
    /// names is skipped.
    #[test]
    fn a_host_is_imported_under_a_name_its_line_applies_to() {
        let dir = tempfile::TempDir::new().unwrap();
        let result = import_text(
            dir.path(),
            "Host !old old backup\n  HostName backup.example.com\nHost !gone gone\n  Port 2\n",
        );
        let got: Vec<(&str, &str)> = result
            .servers
            .iter()
            .map(|s| (s.name.as_str(), s.host.as_str()))
            .collect();
        assert_eq!(got, [("backup", "backup.example.com")]);
        assert_eq!(
            reasons(&result),
            [(
                "!gone",
                "its `Host` line excludes every host it names, and ssh applies it to none of them"
            )]
        );
    }

    /// The advice given with a skip works when followed: a value set above
    /// the `Match` line that may set it imports, one set below it does not
    /// (the first value ssh reads wins); a key that decides DNS or the
    /// second reading, set in the second reading, changes nothing, so it
    /// skips nothing; `CanonicalDomains none` asks DNS for nothing.
    #[test]
    fn a_skip_names_a_remedy_that_imports_the_host() {
        let dir = tempfile::TempDir::new().unwrap();
        let runtime = "Match exec true\n  Port 1\n";
        let skipped = import_text(dir.path(), &format!("{runtime}Host lab\n"));
        assert_eq!(
            reasons(&skipped),
            [("lab", "the `Match` block on line 1 of the file may set its `port`, on a condition known only when ssh runs; give the host its `port` in a `Host` block above that line and import again")]
        );
        let above = import_text(dir.path(), &format!("Host lab\n  Port 2222\n{runtime}"));
        assert_eq!(above.servers.len(), 1);
        assert_eq!(above.servers[0].port, 2222);
        let below = import_text(dir.path(), &format!("{runtime}Host lab\n  Port 2222\n"));
        assert!(below.servers.is_empty());

        let late = "Host lab\n  HostName l\nMatch final localnetwork 192.0.2.0/24\n";
        let read = import_text(
            dir.path(),
            &format!("{late}  CanonicalizeHostname yes\n  CanonicalDomains example.com\n"),
        );
        assert_eq!(read.servers.len(), 1, "{:?}", reasons(&read));
        let port = import_text(dir.path(), &format!("{late}  Port 1\n"));
        assert!(port.servers.is_empty());

        let domains = |value: &str| {
            import_text(
                dir.path(),
                &format!("CanonicalizeHostname yes\nCanonicalDomains {value}\nHost lab\n  HostName l\nHost l\n  Port 2345\n"),
            )
        };
        let none = domains("none");
        let lab = none.servers.iter().find(|s| s.name == "lab");
        assert_eq!(lab.map(|s| s.port), Some(2345), "{:?}", reasons(&none));
        let dns = domains("example.com");
        assert!(dns.servers.iter().all(|s| s.name != "lab"));
        assert!(dns.skipped[0]
            .reason
            .ends_with("import a copy of the file that holds only a `Host` block for it, with its full host name in `HostName` and the `Port`, `User` and `IdentityFile` ssh uses for it, and no `CanonicalizeHostname`, `CanonicalDomains` or `CanonicalizePermittedCNAMEs` line"));
        // The advice, followed, imports the host: a copy with its block
        // alone, the canonical lines left out, even with them global in the
        // file it was copied from.
        for file in [
            "CanonicalizeHostname yes\nCanonicalizePermittedCNAMEs *:*\nHost lab\n  HostName l\n",
            "CanonicalizeHostname yes\nCanonicalDomains example.com\nHost lab\n  HostName l\nHost l\n  Port 2345\n",
        ] {
            let skipped = import_text(dir.path(), file);
            assert!(skipped.servers.is_empty(), "{file}");
            assert!(skipped.skipped[0].reason.contains("import a copy of the file"));
            let copy = import_text(
                dir.path(),
                "Host lab\n  HostName l.example.com\n  Port 2345\n  User ops\n  IdentityFile /k/lab\n",
            );
            let lab = copy.servers.iter().find(|s| s.name == "lab");
            assert_eq!(
                lab.map(|s| (s.host.as_str(), s.port, s.username.as_str())),
                Some(("l.example.com", 2345, "ops")),
                "{:?}",
                reasons(&copy)
            );
        }
    }

    /// A file over a limit with no host block left to carry the reason is
    /// an error, not an empty import; an empty file is an empty import.
    #[test]
    fn a_refused_file_with_no_host_to_report_is_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut text = String::new();
        for i in 0..MAX_HOSTS {
            text.push_str(&format!("Match originalhost m{i}\n"));
        }
        text.push_str("Host lab\n");
        let file = dir.path().join("config");
        std::fs::write(&file, text).unwrap();
        assert_eq!(
            import_ssh_config(&file).err(),
            Some(format!("the file has more than {MAX_HOSTS} `Host` and `Match` blocks, more than one import reads; {SMALLER_FILE}"))
        );
        std::fs::write(&file, "Port 2\nMatch final bogus\n").unwrap();
        assert!(import_ssh_config(&file)
            .err()
            .is_some_and(|e| e.starts_with("line 2 of the file gives `Match` a condition")));
        let empty = import_text(dir.path(), "");
        assert!(empty.servers.is_empty() && empty.skipped.is_empty());
    }

    /// Every byte a pattern compares is a step, on a match as on a miss:
    /// a file of wildcard patterns that all match spends the budget too.
    #[test]
    fn every_byte_a_pattern_compares_is_counted() {
        let name = "a".repeat(1000);
        for (pattern, matches) in [("*", true), ("a*a", true), ("*b", false), ("?*z", false)] {
            let steps = Steps::default();
            assert_eq!(ssh_pattern(pattern, &name, &steps), matches, "{pattern}");
            assert!(steps.0.get() >= 1000, "{pattern}: {}", steps.0.get());
        }
    }

    /// The work of an import is bounded: blocks that name a host apply to
    /// it without matching their names again, so overlapping host lists
    /// stay cheap, and patterns that take more steps than the budget refuse
    /// the file whole; the local account is read once.
    #[test]
    fn the_work_of_an_import_is_bounded() {
        let dir = tempfile::TempDir::new().unwrap();
        // 1,000 blocks, each naming the same 1,000 hosts from a different
        // first one: a million names, each block read by its own hosts only.
        let mut text = String::new();
        for block in 0..1000 {
            let names: Vec<String> = (0..1000)
                .map(|i| format!("h{}", (block + i) % 1000))
                .collect();
            text.push_str(&format!("Host {}\n", names.join(" ")));
        }
        let file = dir.path().join("config");
        std::fs::write(&file, &text).unwrap();
        let config = SshConfig::new(parse_ssh_config(&text).0);
        let start = std::time::Instant::now();
        let (servers, skipped) = import_hosts(&config, None);
        assert_eq!(servers.len(), 1000);
        assert_eq!(skipped.len(), 0);
        let spent = config.steps.0.get();
        assert!(spent < 20_000_000, "{spent} steps");
        eprintln!("overlapping lists: {spent} steps, {:?}", start.elapsed());

        // One long name against one pattern that backtracks over it.
        let name = "a".repeat(100_000);
        let pattern = format!("*{}b", "a".repeat(5_000));
        let text = format!("Host {pattern}\n  Port 2\nHost {name}\n");
        let start = std::time::Instant::now();
        let result = import_text(dir.path(), &text);
        let elapsed = start.elapsed();
        assert!(result.servers.is_empty());
        assert_eq!(
            result.skipped[0].reason,
            format!("matching the file's hosts against its `Host` and `Match` blocks takes more than {MAX_MATCH_STEPS} steps, more than one import spends; {SMALLER_FILE}")
        );
        eprintln!("backtracking pattern: refused in {elapsed:?}");

        // The local account, for every rule and token, is read once.
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("Match localuser nobody{i} user x{i}\n  Port 3\n"));
        }
        for i in 0..50 {
            text.push_str(&format!("Host h{i}\n  IdentityFile /k/%u_%d\n"));
        }
        ACCOUNT_READS.with(|n| n.set(0));
        let result = import_text(dir.path(), &text);
        assert_eq!(result.servers.len(), 50);
        assert_eq!(ACCOUNT_READS.with(|n| n.get()), 1);

        // Every other loop and value the file decides the size of is
        // charged to the same budget, or bounded: grown past the budget, the
        // file is refused whole, and the small version imports.
        let budget = format!("matching the file's hosts against its `Host` and `Match` blocks takes more than {MAX_MATCH_STEPS} steps, more than one import spends; {SMALLER_FILE}");
        let hosts = |n: usize| -> String { (0..n).map(|i| format!("Host h{i}\n")).collect() };
        // `IdentityFile` lines of a block every host applies: one key
        // repeated keeps the list at one entry, the lines are still read.
        let keys = |copies: usize| "IdentityFile /k/one\n".repeat(copies);
        let small = import_text(dir.path(), &format!("{}{}", keys(1000), hosts(10)));
        assert_eq!(small.servers.len(), 10, "{:?}", reasons(&small));
        let start = std::time::Instant::now();
        let grown = import_text(dir.path(), &format!("{}{}", keys(100_000), hosts(2_100)));
        assert!(grown.servers.is_empty());
        assert_eq!(grown.skipped[0].reason, budget);
        eprintln!("repeated keys: refused in {:?}", start.elapsed());
        // The conditions of a `Match` line, read for every host.
        let conditions = |n: usize| format!("Match {}\n  Port 2\n", "!canonical ".repeat(n));
        let small = import_text(dir.path(), &format!("{}{}", hosts(10), conditions(1000)));
        assert!(
            small.servers.iter().all(|s| s.port == 2),
            "{:?}",
            reasons(&small)
        );
        let start = std::time::Instant::now();
        let grown = import_text(
            dir.path(),
            &format!("{}{}", hosts(2_100), conditions(100_000)),
        );
        assert!(grown.servers.is_empty());
        assert_eq!(grown.skipped[0].reason, budget);
        eprintln!("Match conditions: refused in {:?}", start.elapsed());
        // The bytes a `HostName` expands to, read by every `Match host`.
        let alias = "a".repeat(30_000);
        let matches = |n: usize| "Match host x\n  Port 2\n".repeat(n);
        let expanding = |n: usize| {
            format!(
                "Host {alias}\n  HostName %h%h\nHost b{alias}\n  HostName %h%h\n{}",
                matches(n)
            )
        };
        let small = import_text(dir.path(), &expanding(2));
        assert_eq!(small.servers.len(), 2, "{:?}", reasons(&small));
        let start = std::time::Instant::now();
        let grown = import_text(dir.path(), &expanding(2_000));
        assert!(grown.servers.is_empty());
        assert_eq!(grown.skipped[0].reason, budget);
        eprintln!("expanded host names: refused in {:?}", start.elapsed());
        // One value past the longest an expansion gives skips its host
        // only, before it is built: a 50,000-byte alias in 50,000 `%h`.
        let alias = "a".repeat(50_000);
        let text = format!(
            "Host {alias}\n  HostName {}\nHost lab\n  HostName %h.example.com\n",
            "%h".repeat(50_000)
        );
        let start = std::time::Instant::now();
        let result = import_text(dir.path(), &text);
        assert_eq!(result.servers.len(), 1);
        assert_eq!(result.servers[0].host, "lab.example.com");
        assert_eq!(result.skipped[0].reason, format!("its host is longer than {MAX_EXPANDED} bytes once its tokens are expanded, longer than any host name, user name or key path; write it out and import again"));
        eprintln!("oversized expansion: skipped in {:?}", start.elapsed());
        // A name twice on its `Host` line applies the block once: its 100
        // keys are read once, as ssh does, and a 101st is refused.
        let keys: String = (0..100)
            .map(|i| format!("  IdentityFile /k/{i}\n"))
            .collect();
        let twice = import_text(dir.path(), &format!("Host lab lab\n{keys}"));
        assert_eq!(twice.servers.len(), 1, "{:?}", reasons(&twice));
        let over = import_text(
            dir.path(),
            &format!("Host lab lab\n{keys}  IdentityFile /k/100\n"),
        );
        assert!(over.servers.is_empty());
    }

    /// A host name with a `%` or a `:` is kept as written, or in the form
    /// the system gives the address (`ssh -G` on 10.2p1: `fe80::1%LabLink`
    /// and `AB:CD` stay, `fe80:0::1%lo` is `fe80::1%lo`), and the second
    /// reading matches it so.
    #[test]
    fn an_address_like_host_name_is_not_lowercased() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut cases = vec![
            ("fe80::1%%LabLink", "fe80::1%LabLink", "fe80::1%LabLink"),
            ("AB:CD", "AB:CD", "AB:CD"),
            ("Lab%%Link", "Lab%Link", "Lab%Link"),
        ];
        if cfg!(target_os = "linux") {
            cases.push(("fe80:0::1%%lo", "fe80::1%lo", "fe80::1%lo"));
        }
        for (written, host, pattern) in cases {
            let result = import_text(
                dir.path(),
                &format!(
                    "Host lab\n  HostName {written}\nMatch final\nHost {pattern}\n  Port 2345\n"
                ),
            );
            let lab = result.servers.iter().find(|s| s.name == "lab");
            assert_eq!(
                lab.map(|s| (s.host.as_str(), s.port)),
                Some((host, 2345)),
                "{written}: {:?}",
                reasons(&result)
            );
        }
    }
}
