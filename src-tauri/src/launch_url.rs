// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! The connection URL every launch surface reads: the CLI's positional
//! (`aeroftp-cli ls s3://...`), the desktop app's own argument
//! (`aeroftp sftp://...`), and a second launch handed to the running app.
//!
//! [`LAUNCH_SCHEMES`] is the one table of what a URL can name. The CLI and the
//! app both read it, so a scheme one accepts and the other ignores cannot
//! come back (#1534: the app took 8 schemes while the CLI took 19).
//!
//! A URL never has to carry a secret: each protocol reads its key or password
//! from an environment variable ([`LaunchScheme::secret_env`], then
//! `AEROFTP_PASSWORD`). A password typed in the URL is still read, for tools
//! that can pass only one argument, and [`redact`] masks it wherever the URL
//! is logged. A sign-in provider (OAuth and the like) opens only through a
//! saved profile that holds its authorization: no token is ever read from a
//! URL.

pub mod aws;

use std::collections::BTreeMap;

/// How a scheme's account is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchAuth {
    /// User name and password, key or token: from the URL or the environment.
    Server,
    /// A browser sign-in the provider owns. Only a saved, authorized profile
    /// opens it; the URL names which one.
    Account,
}

/// One scheme a launch URL can start with.
#[derive(Debug, PartialEq, Eq)]
pub struct LaunchScheme {
    /// What the URL starts with, lowercase, without `://`.
    pub scheme: &'static str,
    /// The connection manifest token it connects as.
    pub token: &'static str,
    pub auth: LaunchAuth,
    /// An alias is accepted but not listed in help and error texts.
    pub alias: bool,
    /// Environment variables read for the secret, in order, before the
    /// generic `AEROFTP_PASSWORD`.
    pub secret_env: &'static [&'static str],
    /// Environment variables read for the user name (an access key id) when
    /// the URL names none.
    pub user_env: &'static [&'static str],
}

const fn server(
    scheme: &'static str,
    token: &'static str,
    secret_env: &'static [&'static str],
) -> LaunchScheme {
    LaunchScheme {
        scheme,
        token,
        auth: LaunchAuth::Server,
        alias: false,
        secret_env,
        user_env: &[],
    }
}

const fn alias(
    scheme: &'static str,
    token: &'static str,
    secret_env: &'static [&'static str],
) -> LaunchScheme {
    LaunchScheme {
        scheme,
        token,
        auth: LaunchAuth::Server,
        alias: true,
        secret_env,
        user_env: &[],
    }
}

const fn account(scheme: &'static str, token: &'static str, alias: bool) -> LaunchScheme {
    LaunchScheme {
        scheme,
        token,
        auth: LaunchAuth::Account,
        alias,
        secret_env: &[],
        user_env: &[],
    }
}

const FTP_ENV: &[&str] = &["FTP_PASSWORD"];
const SFTP_ENV: &[&str] = &["SFTP_PASSWORD"];
const WEBDAV_ENV: &[&str] = &["WEBDAV_PASSWORD"];

/// Every scheme a launch URL can start with. `file://` is not here: it names
/// a folder of this machine, which each surface opens its own way.
pub static LAUNCH_SCHEMES: &[LaunchScheme] = &[
    server("ftp", "ftp", FTP_ENV),
    server("ftps", "ftps", FTP_ENV),
    // FTP with explicit TLS on the FTP port, FileZilla's spelling.
    server("ftpes", "ftp", FTP_ENV),
    server("sftp", "sftp", SFTP_ENV),
    alias("ssh", "sftp", SFTP_ENV),
    server("scp", "scp", SFTP_ENV),
    server("webdav", "webdav", WEBDAV_ENV),
    server("webdavs", "webdav", WEBDAV_ENV),
    alias("http", "webdav", WEBDAV_ENV),
    alias("https", "webdav", WEBDAV_ENV),
    LaunchScheme {
        scheme: "s3",
        token: "s3",
        auth: LaunchAuth::Server,
        alias: false,
        secret_env: &["AWS_SECRET_ACCESS_KEY"],
        user_env: &["AWS_ACCESS_KEY_ID"],
    },
    server("mega", "mega", &[]),
    server("proton", "proton", &[]),
    alias("protondrive", "proton", &[]),
    server("azure", "azure", &["AZURE_STORAGE_KEY"]),
    server("filen", "filen", &[]),
    server("internxt", "internxt", &[]),
    server("jottacloud", "jottacloud", &[]),
    server("filelu", "filelu", &[]),
    server("koofr", "koofr", &[]),
    server("opendrive", "opendrive", &[]),
    server("github", "github", &[]),
    server("gitlab", "gitlab", &["GITLAB_TOKEN"]),
    server("kdrive", "kdrive", &["KDRIVE_TOKEN"]),
    server("drime", "drime", &["DRIME_TOKEN"]),
    server("swift", "swift", &["ST_KEY", "OS_PASSWORD"]),
    server("immich", "immich", &["IMMICH_API_KEY"]),
    server("imagekit", "imagekit", &["IMAGEKIT_PRIVATE_KEY"]),
    server("uploadcare", "uploadcare", &["UPLOADCARE_SECRET_KEY"]),
    LaunchScheme {
        scheme: "backblaze",
        token: "backblaze",
        auth: LaunchAuth::Server,
        alias: false,
        secret_env: &["B2_APPLICATION_KEY"],
        user_env: &["B2_APPLICATION_KEY_ID"],
    },
    LaunchScheme {
        scheme: "b2",
        token: "backblaze",
        auth: LaunchAuth::Server,
        alias: true,
        secret_env: &["B2_APPLICATION_KEY"],
        user_env: &["B2_APPLICATION_KEY_ID"],
    },
    server("cloudinary", "cloudinary", &["CLOUDINARY_API_SECRET"]),
    // Yandex Disk signs in through OAuth; the CLI also takes its token with
    // `--token` ([`TOKEN_LOGIN_ACCOUNTS`]).
    account("yandexdisk", "yandexdisk", false),
    account("googledrive", "googledrive", false),
    account("drive", "googledrive", true),
    account("dropbox", "dropbox", false),
    account("onedrive", "onedrive", false),
    account("box", "box", false),
    account("pcloud", "pcloud", false),
    account("zohoworkdrive", "zohoworkdrive", false),
    account("fourshared", "fourshared", false),
    account("twake", "twake", false),
];

/// Sign-in providers whose token the CLI also takes on its own (`--token`),
/// so a URL with no saved profile still connects there.
pub const TOKEN_LOGIN_ACCOUNTS: &[&str] = &["yandexdisk"];

/// The fixed API host of a service whose URL names an account, not a server.
pub fn fixed_host(token: &str) -> Option<&'static str> {
    Some(match token {
        "mega" => "mega.nz",
        "proton" => "drive.proton.me",
        "filen" => "gateway.filen.io",
        "internxt" => "drive.internxt.com",
        "jottacloud" => "jfs.jottacloud.com",
        "filelu" => "filelu.com",
        "koofr" => "app.koofr.net",
        "opendrive" => "dev.opendrive.com",
        "yandexdisk" => "cloud-api.yandex.net",
        "kdrive" => "api.infomaniak.com",
        "drime" => "app.drime.cloud",
        "imagekit" => "api.imagekit.io",
        "uploadcare" => "api.uploadcare.com",
        "backblaze" => "api.backblazeb2.com",
        "cloudinary" => "api.cloudinary.com",
        _ => return None,
    })
}

/// The scheme `raw` names, any case.
pub fn launch_scheme(raw: &str) -> Option<&'static LaunchScheme> {
    LAUNCH_SCHEMES
        .iter()
        .find(|entry| entry.scheme.eq_ignore_ascii_case(raw))
}

/// The scheme of `raw`, when it starts with `<scheme>://` and the table has it.
pub fn scheme_of(raw: &str) -> Option<&'static LaunchScheme> {
    let (scheme, _) = raw.split_once("://")?;
    launch_scheme(scheme)
}

/// Whether `raw` is a launch URL of a scheme in the table.
pub fn is_launch_url(raw: &str) -> bool {
    scheme_of(raw).is_some()
}

/// The schemes help and error texts list: every scheme but the aliases.
pub fn listed_schemes() -> Vec<&'static str> {
    LAUNCH_SCHEMES
        .iter()
        .filter(|entry| !entry.alias)
        .map(|entry| entry.scheme)
        .collect()
}

/// Options a URL may set in its query, per connection token, in the spelling
/// the backend reads (`ProviderConfig.extra`). Anything else is refused:
/// a typo would otherwise connect without the setting, and a secret in a
/// query string would reach every log that prints the URL.
fn query_options(token: &str) -> &'static [&'static str] {
    match token {
        "s3" => &["bucket", "region", "endpoint", "path_style"],
        "azure" => &["container"],
        "github" | "gitlab" => &["branch"],
        "immich" => &["scheme"],
        _ => &[],
    }
}

/// Whether a query or option name reads as a secret.
fn secret_like(key: &str) -> bool {
    let key: String = key
        .chars()
        .filter(|c| !matches!(c, '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect();
    [
        "password",
        "passphrase",
        "secret",
        "token",
        "apikey",
        "accesskey",
        "privatekey",
        "key",
        "sig",
    ]
    .iter()
    .any(|word| key.contains(word))
}

/// A launch URL read into its parts. Every text is decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTarget {
    pub scheme: &'static LaunchScheme,
    /// The URL host as written (an account id for some services), empty when
    /// the URL has none.
    pub host: String,
    pub port: Option<u16>,
    /// The user name the URL names, empty for none.
    pub username: String,
    /// The password the URL carries, if any.
    pub url_secret: Option<String>,
    /// The URL path, decoded; `/` for none.
    pub path: String,
    /// Validated query options, in the backend's spelling.
    pub options: BTreeMap<String, String>,
}

fn decode(raw: &str) -> String {
    percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map(|text| text.into_owned())
        .unwrap_or_else(|_| raw.to_string())
}

/// Read `raw` as a launch URL. `Err` names what is wrong without repeating
/// any secret the URL holds.
pub fn parse(raw: &str) -> Result<LaunchTarget, String> {
    let scheme = scheme_of(raw).ok_or_else(|| {
        format!(
            "Unsupported URL scheme. Supported: {}",
            listed_schemes().join(", ")
        )
    })?;
    let url = url::Url::parse(raw).map_err(|e| format!("Invalid URL: {e}"))?;
    // A raw `/`, `?` or `#` in the password ends the authority early: the
    // parser then reads the start of the password as a port (`u:123/x@host`
    // dials host `u`, port 123) and the rest as the path. `user_part_end`
    // states the rule that tells it from a port and a path.
    if url.password().is_none() && mask_raw_password(raw) != raw {
        return Err(
            "The password holds a raw '/', '?' or '#' that must be percent-encoded (%2F, %3F, %23)"
                .to_string(),
        );
    }
    let host = url.host_str().map(decode).unwrap_or_default();
    let needs_host = !matches!(scheme.auth, LaunchAuth::Account)
        && fixed_host(scheme.token).is_none()
        || matches!(
            scheme.token,
            "kdrive" | "backblaze" | "cloudinary" | "imagekit" | "uploadcare"
        );
    if needs_host && host.is_empty() {
        return Err(format!("Missing host in {}:// URL", scheme.scheme));
    }
    let allowed = query_options(scheme.token);
    let mut options = BTreeMap::new();
    for (key, value) in url.query_pairs() {
        let key = key.to_ascii_lowercase().replace('-', "_");
        // The rejected name is never repeated: a token pasted as a bare query
        // (`?ghp_...`) is read as a field name.
        if secret_like(&key) {
            return Err(format!(
                "A URL query never carries a secret: set it in the environment ({}AEROFTP_PASSWORD)",
                scheme
                    .secret_env
                    .iter()
                    .map(|var| format!("{var} or "))
                    .collect::<String>()
            ));
        }
        if !allowed.contains(&key.as_str()) {
            return Err(if allowed.is_empty() {
                format!("{}:// URLs take no query options", scheme.scheme)
            } else {
                format!(
                    "Unknown query option for {}://; accepted: {}",
                    scheme.scheme,
                    allowed.join(", ")
                )
            });
        }
        // An endpoint names a host, never an account: credentials written
        // into it would reach the logs that print the launch URL.
        if key == "endpoint" && value.contains('@') {
            return Err(
                "The endpoint option names a host, never a user or a password: set the keys in the URL user part or the environment"
                    .to_string(),
            );
        }
        if key == "scheme" && !matches!(value.as_ref(), "http" | "https") {
            return Err("scheme must be http or https".to_string());
        }
        if key == "path_style" && !matches!(value.as_ref(), "true" | "false") {
            return Err("path_style must be true or false".to_string());
        }
        options.insert(key, value.into_owned());
    }
    if scheme.scheme == "ftpes" {
        options.insert("tls_mode".to_string(), "explicit".to_string());
    }
    let path = decode(url.path());
    // The parser folds a `..` segment away, `%2E%2E` included; one behind an
    // encoded separator (`..%2F`, `..%5C`) or a NUL survives decoding and
    // would name a folder outside the one written.
    if path.contains('\0') || path.replace('\\', "/").split('/').any(|part| part == "..") {
        return Err(
            "The URL path climbs out of its folder (a '..' behind an encoded separator, or a NUL)"
                .to_string(),
        );
    }
    Ok(LaunchTarget {
        scheme,
        host,
        port: url.port(),
        username: decode(url.username()),
        url_secret: url.password().map(decode),
        path: if path.is_empty() {
            "/".to_string()
        } else {
            path
        },
        options,
    })
}

impl LaunchTarget {
    pub fn token(&self) -> &'static str {
        self.scheme.token
    }

    /// The first non-empty secret among the scheme's variables, then
    /// `AEROFTP_PASSWORD`.
    pub fn env_secret(&self) -> Option<String> {
        first_env(
            self.scheme
                .secret_env
                .iter()
                .copied()
                .chain(["AEROFTP_PASSWORD"]),
        )
    }

    /// An `s3://` URL's credentials and region through the AWS chain (see
    /// [`aws`]): the URL, `AEROFTP_PASSWORD`, the AWS variables, then the
    /// AWS files. A region the endpoint host spells in its preset's
    /// template (`s3.eu-central-2.wasabisys.com`) wins over all of them, as
    /// it does for a saved profile: SigV4 signs with the host's region.
    pub fn s3_credentials(&self) -> aws::S3Credentials {
        let mut credentials = aws::resolve(
            &self.username,
            self.url_secret.as_deref(),
            self.options.get("region").map(String::as_str),
            |name| std::env::var(name).ok(),
            aws::AwsFiles::from_environment,
        );
        if let Some(region) = self.s3_endpoint_region() {
            credentials.region = Some(region);
        }
        credentials
    }

    /// The region an `s3://` URL's endpoint names through its preset's
    /// endpoint template in the connection manifest, if it names one.
    pub fn s3_endpoint_region(&self) -> Option<String> {
        let endpoint = self
            .options
            .get("endpoint")
            .map_or(self.host.as_str(), String::as_str);
        crate::profile_loader::s3_region_from_endpoint(self.s3_provider_id(), endpoint)
    }

    /// The S3 preset an `s3://` URL's endpoint host belongs to (`wasabi`,
    /// `cloudflare-r2`, `custom-s3`, ...), from the shared S3 host table. The
    /// preset's endpoint template then names the region the host carries.
    pub fn s3_provider_id(&self) -> &'static str {
        crate::bridge_shared::map_s3_provider_from_endpoint(
            self.options
                .get("endpoint")
                .map_or(self.host.as_str(), String::as_str),
        )
    }

    /// The user name: the URL's, else the scheme's variable (an access key id).
    pub fn username_or_env(&self) -> String {
        if !self.username.is_empty() {
            return self.username.clone();
        }
        first_env(self.scheme.user_env.iter().copied()).unwrap_or_default()
    }

    /// The account fields a service's URL host stands for, as the backend
    /// reads them: the B2 bucket, the kDrive id, the Cloudinary cloud name.
    /// ImageKit and Uploadcare take their id as the user name instead.
    pub fn host_options(&self) -> BTreeMap<String, String> {
        let key = match self.token() {
            "backblaze" => "bucket",
            "kdrive" => "drive_id",
            "cloudinary" => "cloud_name",
            _ => return BTreeMap::new(),
        };
        BTreeMap::from([(key.to_string(), self.host.clone())])
    }

    /// The user name a service's URL host stands for (ImageKit id, Uploadcare
    /// public key), else the user name of [`Self::username_or_env`].
    pub fn connection_username(&self) -> String {
        match self.token() {
            "imagekit" | "uploadcare" => self.host.clone(),
            _ => self.username_or_env(),
        }
    }

    /// The host the connection dials: the URL's, the service's own API host,
    /// or (Immich) the base URL the server is reached on.
    pub fn connection_host(&self) -> String {
        if let Some(host) = fixed_host(self.token()) {
            return host.to_string();
        }
        if self.token() == "immich" {
            let scheme = self.options.get("scheme").map_or("https", String::as_str);
            return match self.port {
                Some(port) => format!("{scheme}://{}:{port}", url_host(&self.host)),
                None => format!("{scheme}://{}", url_host(&self.host)),
            };
        }
        self.host.clone()
    }
}

/// The branch a repository URL names: its `@branch` suffix, or `?branch=`.
/// The two naming different branches is refused.
fn repository_branch(
    target: &LaunchTarget,
    suffix: Option<&str>,
) -> Result<Option<String>, String> {
    let query = target
        .options
        .get("branch")
        .map(String::as_str)
        .filter(|branch| !branch.is_empty());
    match (suffix, query) {
        (Some(suffix), Some(query)) if suffix != query => Err(format!(
            "The URL names two branches, @{suffix} and ?branch={query}: keep one"
        )),
        (suffix, query) => Ok(suffix.or(query).map(str::to_string)),
    }
}

/// The repository a `github://` URL names, `owner/repo`, and its branch
/// (an `@branch` suffix or `?branch=`): `github://owner/repo[@branch]`, or
/// `github://github.com/owner/repo[@branch]`.
pub fn github_repository(target: &LaunchTarget) -> Result<(String, Option<String>), String> {
    const SHAPE: &str = "GitHub URL must be github://owner/repo or github://owner/repo@branch";
    // The branch is everything after the `@`, slashes included: Git branch
    // names hold them (`release/2026`, `aeroftp/user/main`).
    let (path, branch) = match target.path.split_once('@') {
        Some((path, branch)) => {
            let branch = branch.trim_end_matches('/');
            if branch.is_empty() {
                return Err(SHAPE.to_string());
            }
            (path, Some(branch))
        }
        None => (target.path.as_str(), None),
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let (owner, repo) = if target.host.eq_ignore_ascii_case("github.com") {
        match segments.as_slice() {
            [owner, repo] => (*owner, *repo),
            [owner, repo, ..] if branch.is_none() => (*owner, *repo),
            _ => return Err(SHAPE.to_string()),
        }
    } else {
        match segments.as_slice() {
            [repo] if !target.host.is_empty() => (target.host.as_str(), *repo),
            [repo, ..] if !target.host.is_empty() && branch.is_none() => {
                (target.host.as_str(), *repo)
            }
            _ => return Err(SHAPE.to_string()),
        }
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(SHAPE.to_string());
    }
    let branch = repository_branch(target, branch)?;
    Ok((format!("{owner}/{repo}"), branch))
}

/// The project a `gitlab://` URL names, in the `host/group/project` form the
/// GitLab backend reads as its host, and its branch (an `@branch` suffix or
/// `?branch=`): `gitlab://gitlab.com/group[/subgroup]/project[@branch]`. The
/// whole path is the project, so the URL opens it at its root. A port stays
/// in the host (`git.example.com:8443/group/project`), the only place the
/// backend reads one, and a host without a dot is refused: the backend would
/// read it as a project owner on gitlab.com and send the token there.
pub fn gitlab_project(target: &LaunchTarget) -> Result<(String, Option<String>), String> {
    const SHAPE: &str =
        "GitLab URL must be gitlab://host/group/project or gitlab://host/group/project@branch";
    let project = target.path.trim_matches('/');
    // The branch is everything after the first `@`, slashes included: a
    // project path holds no `@`, a branch name may hold `/`.
    let (project, branch) = match project.split_once('@') {
        Some((project, branch)) if !project.is_empty() && !branch.is_empty() => {
            (project, Some(branch))
        }
        Some(_) => return Err(SHAPE.to_string()),
        None => (project, None),
    };
    if target.host.is_empty() || !project.contains('/') {
        return Err(SHAPE.to_string());
    }
    if !target.host.contains('.') {
        return Err(format!(
            "GitLab URL host '{}' must be the instance's full name (gitlab.com, git.example.com): \
             a name without a dot would be read as a project on gitlab.com",
            target.host
        ));
    }
    let branch = repository_branch(target, branch)?;
    let host = match target.port {
        Some(port) => format!("{}:{port}", target.host),
        None => target.host.clone(),
    };
    Ok((format!("{host}/{project}"), branch))
}

/// A working example URL for a scheme, as the help and the docs show it.
/// The CLI and app tests read every scheme through it, so both surfaces are
/// proven on the same URL.
pub fn example_url(entry: &LaunchScheme) -> String {
    let s = entry.scheme;
    if entry.auth == LaunchAuth::Account {
        return format!("{s}:///Documents/");
    }
    if let Some(host) = fixed_host(entry.token).filter(|_| {
        !matches!(
            entry.token,
            "kdrive" | "backblaze" | "cloudinary" | "imagekit" | "uploadcare"
        )
    }) {
        return format!("{s}://you%40example.com@{host}/Documents/");
    }
    match entry.token {
        "ftp" | "ftps" => format!("{s}://user@ftp.example.com/pub/"),
        "sftp" | "scp" => format!("{s}://user@host.example.com/home/user/"),
        "webdav" => format!("{s}://user@dav.example.com/remote.php/dav/files/user/"),
        "s3" => {
            format!("{s}://ACCESSKEY@s3.example.com/backups/?bucket=my-bucket&region=eu-central-1")
        }
        "azure" => {
            format!("{s}://account@account.blob.core.windows.net/backups/?container=my-container")
        }
        "swift" => format!("{s}://user@auth.example.com/backups/"),
        "immich" => format!("{s}://photos.example.com:2283/"),
        "gitlab" => format!("{s}://gitlab.com/owner/repo"),
        "github" => format!("{s}://owner/repo"),
        "kdrive" => format!("{s}://123456/Documents/"),
        "backblaze" => format!("{s}://KEYID@my-bucket/backups/"),
        "cloudinary" => format!("{s}://APIKEY@my-cloud/assets/"),
        "imagekit" => format!("{s}://my_imagekit_id/assets/"),
        "uploadcare" => format!("{s}://PUBLICKEY/"),
        other => format!("{s}://{other}.example.com/"),
    }
}

/// A host as it goes back into a URL: an IPv6 address in brackets.
fn url_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

fn first_env<'a>(vars: impl Iterator<Item = &'a str>) -> Option<String> {
    vars.filter_map(|var| std::env::var(var).ok())
        .find(|value| !value.is_empty())
}

/// `raw` with its password and every secret-like query value masked, safe to
/// log or show.
pub fn redact(raw: &str) -> String {
    // The password is masked on the raw text first: a raw `?`, `#`, `/` or
    // `@` in it ends the authority early, and the parser then reads part of
    // the password as a port or a path (an all-digit one even parses).
    let masked = mask_port_path_at(&mask_raw_password(raw));
    let raw = masked.as_str();
    if let Ok(mut url) = url::Url::parse(raw) {
        if url.password().is_some() {
            let _ = url.set_password(Some("***"));
        }
        if url.query().is_some() {
            let pairs: Vec<(String, String)> = url
                .query_pairs()
                .map(|(key, value)| {
                    let value = if secret_like(&key) || (key == "endpoint" && value.contains('@')) {
                        "***".to_string()
                    } else {
                        value.into_owned()
                    };
                    (key.into_owned(), value)
                })
                .collect();
            url.query_pairs_mut().clear().extend_pairs(pairs);
        }
        return url.to_string();
    }
    // Not a URL the parser takes: the password is already masked; drop the
    // query and the fragment, which may hold a secret too.
    raw.split(['?', '#']).next().unwrap_or_default().to_string()
}

/// Where the user part of `rest` (the text after `://`) ends: the index of
/// the `@` before the host, if the text has a user part.
///
/// The authority ends at the first `/`, `?` or `#`. An `@` in it ends the
/// user part, the last one, so a raw `@` in a password stays in it. With no
/// `@` there, a `:` in the authority may still be a password cut short by a
/// raw `/`, `?` or `#`: then an `@` in the very next segment ends the user
/// part (`u:12/x@host/`, which the URL parser reads as host `u`, port 12). An
/// `@` further on is part of the path (`host:2222/photos/a@2x.png`).
///
/// The two limits of that rule, both refused or read as written: a password
/// with two raw `/` before its `@` (`u:12/x/y@host`) reads as a path, and an
/// explicit port followed straight by a name with an `@`
/// (`host:2222/a@2x.png`) reads as a user part; encode them (`%2F`, `%40`).
fn user_part_end(rest: &str) -> Option<usize> {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if let Some(at) = rest[..end].rfind('@') {
        return Some(at);
    }
    if end == rest.len() || !rest[..end].contains(':') {
        return None;
    }
    let after = &rest[end + 1..];
    let segment_end = after.find('/').unwrap_or(after.len());
    after[..segment_end].rfind('@').map(|at| end + 1 + at)
}

/// For the logs only: `raw` with everything from the port to the last `@`
/// masked, when the authority has a port and no `@` and an `@` follows it.
///
/// `parse` reads `u:12/x/y@host` as written (host `u`, port 12, a path), the
/// limit `user_part_end` documents, so the text may still hold a password cut
/// short by a raw `/`. A log line cannot tell that from a valid
/// `host:2222/photos/a@2x.png` either, and masking a valid link costs only
/// the readability of one log line: both log as `<host>:***@<rest>`.
fn mask_port_path_at(raw: &str) -> String {
    let Some(start) = raw.find("://").map(|index| index + 3) else {
        return raw.to_string();
    };
    let rest = &raw[start..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let Some(colon) = authority.rfind(':') else {
        return raw.to_string();
    };
    let port = &authority[colon + 1..];
    if authority.contains('@') || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return raw.to_string();
    }
    let Some(at) = rest[end..].rfind('@').map(|at| end + at) else {
        return raw.to_string();
    };
    format!(
        "{}{}:***{}",
        &raw[..start],
        &authority[..colon],
        &rest[at..]
    )
}

/// `raw` with the password of its user part replaced by `***`. The user part
/// is what `user_part_end` finds, so an `@` in the password stays in it and
/// one in the path stays out. A user name holds no `/`, `?` or `#`, so a `:`
/// in the path or the query of a URL without a user part is never taken for
/// one.
fn mask_raw_password(raw: &str) -> String {
    let Some(start) = raw.find("://").map(|index| index + 3) else {
        return raw.to_string();
    };
    let rest = &raw[start..];
    let Some(at) = user_part_end(rest) else {
        return raw.to_string();
    };
    let Some(colon) = rest[..at].find(':') else {
        return raw.to_string();
    };
    // `user:@host` (anonymous FTP) has no password to mask: the user part is
    // read, and the secret comes from the environment or the prompt.
    if rest[..colon].contains(['/', '?', '#']) || colon + 1 == at {
        return raw.to_string();
    }
    format!("{}{}:***{}", &raw[..start], &rest[..colon], &rest[at..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_scheme_connects_as_a_manifest_protocol() {
        for entry in LAUNCH_SCHEMES {
            let spec = crate::connection_manifest::connection_spec(entry.token)
                .unwrap_or_else(|| panic!("{} has no manifest protocol", entry.scheme));
            assert_eq!(spec.token, entry.token, "{}", entry.scheme);
            let account = matches!(
                spec.credential_kind.as_str(),
                "oauth2" | "oauth1" | "sign_in"
            );
            assert_eq!(
                account,
                entry.auth == LaunchAuth::Account,
                "{} is {} in the manifest",
                entry.scheme,
                spec.credential_kind
            );
        }
    }

    #[test]
    fn every_profile_protocol_has_a_launch_scheme_or_a_stated_reason() {
        // Managed and runtime connections have no URL of their own; Google
        // Photos is a DEV-only protocol.
        let no_url = [
            "aerocloud",
            "peer",
            "mtp",
            "local",
            "aerovaultmount",
            "googlephotos",
        ];
        for spec in &crate::connection_manifest::manifest().protocols {
            if no_url.contains(&spec.token.as_str()) {
                continue;
            }
            assert!(
                LAUNCH_SCHEMES.iter().any(|entry| entry.token == spec.token),
                "{} has no launch scheme",
                spec.token
            );
        }
    }

    #[test]
    fn every_example_url_parses_as_its_own_scheme() {
        for entry in LAUNCH_SCHEMES {
            let target =
                parse(&example_url(entry)).unwrap_or_else(|e| panic!("{}: {e}", entry.scheme));
            assert_eq!(target.scheme, entry, "{}", entry.scheme);
        }
    }

    #[test]
    fn schemes_are_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for entry in LAUNCH_SCHEMES {
            assert_eq!(entry.scheme, entry.scheme.to_ascii_lowercase());
            assert!(seen.insert(entry.scheme), "{} twice", entry.scheme);
        }
    }

    #[test]
    fn a_url_reads_into_decoded_parts() {
        let target = parse("SFTP://me%40corp:p%40ss@host.example:2222/srv/a%20b/").unwrap();
        assert_eq!(target.token(), "sftp");
        assert_eq!(target.host, "host.example");
        assert_eq!(target.port, Some(2222));
        assert_eq!(target.username, "me@corp");
        assert_eq!(target.url_secret.as_deref(), Some("p@ss"));
        assert_eq!(target.path, "/srv/a b/");
    }

    #[test]
    fn s3_takes_bucket_region_and_endpoint_from_the_query() {
        let target =
            parse("s3://AKID@s3.eu-central-1.amazonaws.com/photos/?bucket=b1&region=eu-central-1")
                .unwrap();
        assert_eq!(target.connection_host(), "s3.eu-central-1.amazonaws.com");
        assert_eq!(target.options.get("bucket").map(String::as_str), Some("b1"));
        assert_eq!(
            target.options.get("region").map(String::as_str),
            Some("eu-central-1")
        );
        assert_eq!(target.connection_username(), "AKID");
        assert_eq!(target.path, "/photos/");
    }

    #[test]
    fn a_secret_or_an_unknown_option_in_the_query_is_refused_without_echoing_it() {
        for raw in [
            "s3://AKID@host/?secret=hunter2",
            "s3://AKID@host/?aws_secret_access_key=hunter2",
            "immich://host/?api-key=hunter2",
            "gitlab://gitlab.com/o/r?private_token=hunter2",
        ] {
            let error = parse(raw).unwrap_err();
            assert!(!error.contains("hunter2"), "{raw}: {error}");
            assert!(error.contains("environment"), "{raw}: {error}");
        }
        // The rejected name is never repeated either: a token pasted as a
        // bare query is read as a field name.
        let error = parse("s3://AKID@host/?bukcet=b").unwrap_err();
        assert!(
            !error.contains("bukcet") && error.contains("bucket"),
            "{error}"
        );
        for raw in [
            "s3://AKID@host/?ghp_dummy0123456789",
            "sftp://host/?ghp_dummy0123456789",
            "s3://AKID@host/?mytoken0123=1",
        ] {
            let error = parse(raw).unwrap_err();
            assert!(!error.contains("dummy0123"), "{raw}: {error}");
            assert!(!error.contains("mytoken0123"), "{raw}: {error}");
        }
        assert!(parse("sftp://host/?region=x").is_err());
    }

    #[test]
    fn an_s3_endpoint_never_carries_credentials() {
        let raw = "s3://AKID@host/?bucket=b&endpoint=https://u:hunter2@minio.example.com";
        let error = parse(raw).unwrap_err();
        assert!(!error.contains("hunter2"), "{error}");
        let masked = redact(raw);
        assert!(!masked.contains("hunter2"), "{masked}");
        assert!(masked.contains("bucket=b"), "{masked}");
        // A custom endpoint is read and logged as written.
        let raw = "s3://AKID@host/?bucket=b&endpoint=https://minio.example.com:9000";
        assert_eq!(
            parse(raw)
                .unwrap()
                .options
                .get("endpoint")
                .map(String::as_str),
            Some("https://minio.example.com:9000")
        );
        assert!(redact(raw).contains("minio.example.com"));
    }

    #[test]
    fn services_read_their_account_from_the_host() {
        let b2 = parse("b2://KEYID@my-bucket/backups/").unwrap();
        assert_eq!(b2.token(), "backblaze");
        assert_eq!(b2.connection_host(), "api.backblazeb2.com");
        assert_eq!(b2.connection_username(), "KEYID");
        assert_eq!(
            b2.host_options().get("bucket").map(String::as_str),
            Some("my-bucket")
        );

        let cloudinary = parse("cloudinary://123456:abc@demo/").unwrap();
        assert_eq!(cloudinary.connection_username(), "123456");
        assert_eq!(cloudinary.url_secret.as_deref(), Some("abc"));
        assert_eq!(
            cloudinary
                .host_options()
                .get("cloud_name")
                .map(String::as_str),
            Some("demo")
        );

        let kdrive = parse("kdrive://123456/Documents").unwrap();
        assert_eq!(
            kdrive.host_options().get("drive_id").map(String::as_str),
            Some("123456")
        );
        assert_eq!(kdrive.connection_host(), "api.infomaniak.com");

        let imagekit = parse("imagekit://my_id/assets").unwrap();
        assert_eq!(imagekit.connection_username(), "my_id");
        assert_eq!(imagekit.connection_host(), "api.imagekit.io");

        let uploadcare = parse("uploadcare://pubkey/").unwrap();
        assert_eq!(uploadcare.connection_username(), "pubkey");

        let drime = parse("drime:///Projects").unwrap();
        assert_eq!(drime.connection_host(), "app.drime.cloud");
        assert_eq!(drime.path, "/Projects");

        for raw in ["b2:///x", "kdrive:///x", "cloudinary:///x"] {
            assert!(parse(raw).is_err(), "{raw}");
        }
    }

    #[test]
    fn immich_dials_https_unless_the_url_asks_for_http() {
        assert_eq!(
            parse("immich://photos.example:2283/")
                .unwrap()
                .connection_host(),
            "https://photos.example:2283"
        );
        assert_eq!(
            parse("immich://192.168.1.5:2283/?scheme=http")
                .unwrap()
                .connection_host(),
            "http://192.168.1.5:2283"
        );
        assert!(parse("immich://host/?scheme=ftp").is_err());
    }

    #[test]
    fn ftpes_is_ftp_with_explicit_tls() {
        let target = parse("ftpes://user@ftp.example/").unwrap();
        assert_eq!(target.token(), "ftp");
        assert_eq!(
            target.options.get("tls_mode").map(String::as_str),
            Some("explicit")
        );
    }

    #[test]
    fn a_sign_in_provider_needs_no_host() {
        let target = parse("dropbox:///Photos").unwrap();
        assert_eq!(target.scheme.auth, LaunchAuth::Account);
        assert_eq!(target.host, "");
        let named = parse("googledrive://work/Reports").unwrap();
        assert_eq!(named.token(), "googledrive");
        assert_eq!(named.host, "work");
    }

    #[test]
    fn an_encoded_parent_folder_or_nul_in_the_path_is_refused() {
        for raw in [
            "sftp://u@h/home/..%5Cetc",
            "sftp://u@h/home/%5C..%5Cetc",
            "s3://AK@h/a%2F..%2Fb?bucket=x",
            "webdav://u@h/a%00b",
            "dropbox:///x%2F..%2F..%2Fy",
        ] {
            assert!(parse(raw).is_err(), "{raw}");
        }
        // The URL parser folds a `..` segment, literal or `%2E%2E`, inside
        // the path the URL names: it never climbs above the root.
        assert_eq!(parse("sftp://u@h/a/../b").unwrap().path, "/b");
        assert_eq!(parse("sftp://u@h/home/%2E%2E/etc").unwrap().path, "/etc");
        assert_eq!(parse("sftp://u@h/%2E%2E/%2E%2E/etc").unwrap().path, "/etc");
        assert_eq!(parse("sftp://u@h/a..b/").unwrap().path, "/a..b/");
    }

    #[test]
    fn unknown_schemes_and_hostless_servers_are_refused() {
        assert!(parse("gopher://host").is_err());
        assert!(parse("sftp:///home").is_err());
        assert!(!is_launch_url("file:///tmp"));
        assert!(!is_launch_url("C:\\Users"));
        assert!(is_launch_url("B2://k@b/"));
    }

    #[test]
    fn github_names_a_repository_and_an_optional_branch() {
        let repo = |raw: &str| github_repository(&parse(raw).unwrap());
        assert_eq!(
            repo("github://t@owner/repo").unwrap(),
            ("owner/repo".into(), None)
        );
        assert_eq!(
            repo("github://t@owner/repo@main").unwrap(),
            ("owner/repo".into(), Some("main".into()))
        );
        // A branch keeps its slashes; nothing after the `@` is dropped.
        assert_eq!(
            repo("github://github.com/owner/repo@dev/sub").unwrap(),
            ("owner/repo".into(), Some("dev/sub".into()))
        );
        assert_eq!(
            repo("github://t@owner/repo@release/2026").unwrap(),
            ("owner/repo".into(), Some("release/2026".into()))
        );
        assert!(repo("github://t@owner/repo/sub@dev").is_err());
        assert!(repo("github://t@owner/repo@").is_err());
        assert!(repo("github://owner").is_err());
        assert!(repo("github://github.com/owner").is_err());
    }

    #[test]
    fn a_branch_query_names_the_branch_unless_the_suffix_disagrees() {
        let repo = |raw: &str| github_repository(&parse(raw).unwrap());
        assert_eq!(
            repo("github://t@owner/repo?branch=dev").unwrap(),
            ("owner/repo".into(), Some("dev".into()))
        );
        assert_eq!(
            repo("github://t@owner/repo@dev?branch=dev").unwrap(),
            ("owner/repo".into(), Some("dev".into()))
        );
        assert!(repo("github://t@owner/repo@main?branch=dev").is_err());
    }

    #[test]
    fn gitlab_names_a_project_in_the_host_and_an_optional_branch() {
        let project = |raw: &str| gitlab_project(&parse(raw).unwrap());
        assert_eq!(
            project("gitlab://gitlab.com/owner/repo").unwrap(),
            ("gitlab.com/owner/repo".into(), None)
        );
        assert_eq!(
            project("gitlab://git.example.com/group/sub/project@dev").unwrap(),
            (
                "git.example.com/group/sub/project".into(),
                Some("dev".into())
            )
        );
        assert_eq!(
            project("gitlab://gitlab.com/owner/repo?branch=dev").unwrap(),
            ("gitlab.com/owner/repo".into(), Some("dev".into()))
        );
        assert!(project("gitlab://gitlab.com/owner/repo@main?branch=dev").is_err());
        assert_eq!(
            project("gitlab://gitlab.com/owner/repo@release/2026").unwrap(),
            ("gitlab.com/owner/repo".into(), Some("release/2026".into()))
        );
        assert!(project("gitlab://gitlab.com/").is_err());
        assert!(project("gitlab://gitlab.com/owner").is_err());
    }

    #[test]
    fn a_gitlab_url_sends_the_token_only_to_the_instance_it_names() {
        use crate::providers::{gitlab::GitLabConfig, ProviderConfig, ProviderType};
        let api_base = |raw: &str| {
            let (host, _) = gitlab_project(&parse(raw).unwrap())?;
            let config = ProviderConfig {
                name: "GitLab".into(),
                provider_type: ProviderType::GitLab,
                host,
                port: None,
                username: None,
                password: Some("glpat-test".into()),
                initial_path: Some("/".into()),
                extra: Default::default(),
            };
            GitLabConfig::from_provider_config(&config)
                .map(|config| (config.api_base, config.project_path))
                .map_err(|e| e.to_string())
        };
        assert_eq!(
            api_base("gitlab://git.example.com:8443/group/project").unwrap(),
            (
                "https://git.example.com:8443/api/v4".into(),
                "group/project".into()
            )
        );
        assert_eq!(
            api_base("gitlab://gitlab.com/group/project").unwrap(),
            ("https://gitlab.com/api/v4".into(), "group/project".into())
        );
        assert!(api_base("gitlab://gitlab/group/project").is_err());
        assert!(api_base("gitlab://gitlab:8443/group/project").is_err());
    }

    #[test]
    fn redact_masks_the_password_and_secret_query_values() {
        assert_eq!(redact("sftp://u:hunter2@h/p"), "sftp://u:***@h/p");
        let masked = redact("s3://AKID@h/?bucket=b&secret=hunter2");
        assert!(
            !masked.contains("hunter2") && masked.contains("bucket=b"),
            "{masked}"
        );
        assert_eq!(redact("sftp://u@h/p"), "sftp://u@h/p");
        // Not parseable: the user part is still masked and the query dropped.
        let masked = redact("sftp://u:hunter2@[bad/?token=x");
        assert!(
            !masked.contains("hunter2") && !masked.contains("token=x"),
            "{masked}"
        );
    }

    #[test]
    fn redact_masks_a_password_whatever_characters_it_holds() {
        // A password with a raw `?`, `#`, `/` or `@` ends the authority
        // early, so the parser reads part of it as a port or a path: the
        // first fallback cut the query before looking for the `@` and logged
        // `ftp://u:pa`. An all-digit password even parses, as host `u` and
        // port 123, and left the parser's path with no password to mask.
        for (raw, kept) in [
            ("ftp://u:pa?ss@host/", "ftp://u:***@host/"),
            ("ftp://u:pa#ss@host/", "ftp://u:***@host/"),
            ("ftp://u:pa/ss@host/", "ftp://u:***@host/"),
            ("ftp://u:p@ss@host/", "ftp://u:***@host/"),
            ("ftp://u:123?x@host/", "ftp://u:***@host/"),
            (
                "s3://KEY:abc?def@host/x?bucket=b",
                "s3://KEY:***@host/x?bucket=b",
            ),
            (
                "sftp://u:pw@host/file@2x.png",
                "sftp://u:***@host/file@2x.png",
            ),
        ] {
            assert_eq!(redact(raw), kept, "{raw}");
        }
        // A port and a path with an `@` further on is a link, not a password:
        // read and logged as written.
        let target = parse("sftp://files.example:2222/photos/a@2x.png").unwrap();
        assert_eq!(
            (target.host.as_str(), target.port, target.path.as_str()),
            ("files.example", Some(2222), "/photos/a@2x.png")
        );
        assert_eq!(target.url_secret, None);
        // In a log it is masked all the same: it has the shape of a
        // password cut short by a raw `/` (`u:12/x/y@host` below).
        assert_eq!(
            redact("sftp://files.example:2222/photos/a@2x.png"),
            "sftp://files.example:***@2x.png"
        );
        // The rule's limit, refused rather than dialed: an `@` in the first
        // name after a port reads as the end of a user part; `%40` opens it.
        assert!(parse("sftp://h:2222/a@2x.png").is_err());
        assert_eq!(parse("sftp://h:2222/a%402x.png").unwrap().port, Some(2222));
        // The read-as-path limit: parsed as written, never logged with the
        // password it may hold.
        let target = parse("ftp://u:12/x/y@host/").unwrap();
        assert_eq!(
            (target.host.as_str(), target.port, target.path.as_str()),
            ("u", Some(12), "/x/y@host/")
        );
        let masked = redact("ftp://u:12/x/y@host/");
        assert_eq!(masked, "ftp://u:***@host/");
        assert!(!masked.contains("12/x/y"), "{masked}");
        // A port without an `@` after it, or a user part, is logged as is.
        assert_eq!(redact("sftp://h:2222/var/log"), "sftp://h:2222/var/log");
        assert_eq!(
            redact("sftp://alice@h:2222/a@2x.png"),
            "sftp://alice@h:2222/a@2x.png"
        );
        // A raw `@` in the password is the URL standard's case: the last `@`
        // of the authority ends the user part.
        assert_eq!(
            parse("ftp://u:p@ss@host/").unwrap().url_secret.as_deref(),
            Some("p@ss")
        );
        // Read for a connection, a password the parser would split is
        // refused, never dialed as a host and port.
        for (raw, secret) in [
            ("ftp://u:123/x@host/", "123"),
            ("sftp://user:12/x@host/", "12/x"),
            ("ftp://u:123?x@host/", "123"),
            ("ftp://u:pa#ss@host/", "pa#ss"),
        ] {
            let error = parse(raw).err().unwrap_or_else(|| panic!("{raw} parsed"));
            assert!(!error.contains(secret), "{raw}: {error}");
        }
        assert_eq!(
            parse("ftp://u:p%2Fss@host/").unwrap().url_secret.as_deref(),
            Some("p/ss")
        );
        // An empty password is no password: read as main read it, and
        // nothing to mask.
        for raw in ["ftp://anonymous:@host/pub", "ftp://u:@host/"] {
            let target = parse(raw).unwrap_or_else(|e| panic!("{raw}: {e}"));
            assert_eq!(target.url_secret, None, "{raw}");
            assert!(!redact(raw).contains("***"), "{raw}");
        }
        assert_eq!(
            parse("ftp://anonymous:@host/pub").unwrap().username,
            "anonymous"
        );
        // A limit, not a leak this code can close: `u:a@b/c@host` is, by the
        // URL standard, password `a` at host `b` with path `/c@host`. An `@`
        // followed by a `/` in a password cannot be told from an `@` in the
        // path (`file@2x.png`) without breaking the path; encode it as `%40`.
        // No password: nothing in the path or the query is taken for one.
        assert_eq!(redact("sftp://alice@host/a@b"), "sftp://alice@host/a@b");
        assert_eq!(
            redact("s3://host/x?bucket=b&note=a:b@c"),
            "s3://host/x?bucket=b&note=a%3Ab%40c"
        );
        assert_eq!(
            redact("github://t@owner/repo@main"),
            "github://t@owner/repo@main"
        );
        // A URL that does not parse at all keeps no secret either.
        let masked = redact("sftp://u:hunter2@[bad/?token=x#frag");
        assert!(
            !masked.contains("hunter2") && !masked.contains("token") && !masked.contains("frag"),
            "{masked}"
        );
    }

    #[test]
    fn the_scheme_secret_variable_wins_over_the_generic_one() {
        let _guard = crate::test_env::lock();
        let target = parse("immich://host/").unwrap();
        std::env::set_var("AEROFTP_PASSWORD", "generic");
        std::env::remove_var("IMMICH_API_KEY");
        assert_eq!(target.env_secret().as_deref(), Some("generic"));
        std::env::set_var("IMMICH_API_KEY", "specific");
        assert_eq!(target.env_secret().as_deref(), Some("specific"));
        std::env::set_var("IMMICH_API_KEY", "");
        assert_eq!(target.env_secret().as_deref(), Some("generic"));
        std::env::remove_var("IMMICH_API_KEY");
        std::env::remove_var("AEROFTP_PASSWORD");
        assert_eq!(target.env_secret(), None);

        let s3 = parse("s3://h/?bucket=b").unwrap();
        std::env::set_var("AWS_ACCESS_KEY_ID", "AKENV");
        assert_eq!(s3.connection_username(), "AKENV");
        assert_eq!(
            parse("s3://AKURL@h/").unwrap().connection_username(),
            "AKURL"
        );
        std::env::remove_var("AWS_ACCESS_KEY_ID");
    }
}
