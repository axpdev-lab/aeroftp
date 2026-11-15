// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

//! Configuration Template parser preview and atomic batch profile import.
//!
//! Shares backend mapping and key-file expansion with [`crate::rclone_import`]
//! via [`crate::rclone_import::evaluate_template_text`], while keeping legacy
//! file-import behavior unchanged and guaranteeing zero secret leakage across
//! IPC previews, diagnostics, and logs.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::bridge_shared::MAX_IMPORT_FILE_BYTES;
use crate::credential_store::CredentialStore;
use crate::profile_export::{ProviderSecrets, ServerProfileExport};
use crate::profile_import_plan::{
    lay_import_over, same_target, write_secrets_and_commit, ExistingProfileMode, ImportedSecurity,
    SecretCommit, SecretSlots, SecretWrite,
};
use crate::profile_security::{
    make_parts_safe, new_relaxations, security_relaxations_of, KeptRelaxation, RelaxationPolicy,
};
use crate::rclone_import::{
    evaluate_template_text, IniParseIssue, TemplateBlockWarning, TemplateEngineOutput,
    TemplateSecretForm, TemplateSecretPolicy, TemplateSkippedBlock,
};
use crate::user_partitions;
use crate::util::profile_name::ProfileNames;

/// User-supplied configuration template source.
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TemplateInput {
    /// Raw INI text pasted into the editor or read from stdin (`-`).
    Text { content: String },
    /// Filesystem path to a template file (`key_file` variables expand against its directory).
    File { path: String },
}

// Redact raw template content in Debug output so secrets never leak to traces.
impl fmt::Debug for TemplateInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { content } => f
                .debug_struct("TemplateInput::Text")
                .field("bytes", &content.len())
                .finish(),
            Self::File { path } => f
                .debug_struct("TemplateInput::File")
                .field("path", path)
                .finish(),
        }
    }
}

/// Preview parameters shared across GUI IPC and CLI dry-run.
#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePreviewOptions {
    #[serde(default)]
    pub existing: ExistingProfileMode,
    #[serde(default)]
    pub default_secret_form: Option<TemplateSecretForm>,
    #[serde(default)]
    pub secret_forms: HashMap<String, TemplateSecretForm>,
}

/// Apply parameters shared across GUI IPC and CLI `--apply`.
#[derive(Clone, Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TemplateApplyOptions {
    #[serde(default)]
    pub existing: ExistingProfileMode,
    #[serde(default)]
    pub default_secret_form: Option<TemplateSecretForm>,
    #[serde(default)]
    pub secret_forms: HashMap<String, TemplateSecretForm>,
    /// Optional subset of trimmed block names selected for import. `None` imports all valid blocks.
    #[serde(default)]
    pub selected_blocks: Option<Vec<String>>,
    /// Security-relaxing option keys explicitly approved by the user or caller.
    #[serde(default)]
    pub keep_relaxed: Vec<KeptRelaxation>,
    /// SHA-256 hex digest computed during preview; when set, apply fails if the input changed.
    #[serde(default)]
    pub expected_digest_sha256: Option<String>,
    /// SHA-256 hex digest of the previewed import plan; when set, apply fails if saved profiles changed.
    #[serde(default)]
    pub expected_plan_digest_sha256: Option<String>,
}

/// Action planned for a single mapped template block against the active user's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum TemplatePlannedAction {
    Add,
    Skip,
    Rename,
    Replace,
}

/// What happens to the credential slot of a planned block on import.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SecretDisposition {
    None,
    NewCredential,
    KeptExisting,
    ClearedExisting,
}

/// Redacted preview row for one mapped block in a configuration template.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePreviewRow {
    pub file_index: usize,
    pub block_name: String,
    pub target_name: String,
    pub start_line: usize,
    pub protocol: String,
    pub provider_id: Option<String>,
    pub host: String,
    pub port: u32,
    pub username: String,
    pub has_credential: bool,
    pub has_secret_input: bool,
    pub looks_obscured: bool,
    pub secret_form: Option<TemplateSecretForm>,
    pub needs_secret_form_choice: bool,
    pub existing_match_id: Option<String>,
    pub existing_match_name: Option<String>,
    pub action: TemplatePlannedAction,
    pub duplicate_in_template: bool,
    pub secret_disposition: SecretDisposition,
    pub secret_note: Option<String>,
}

/// Full redacted preview report returned to GUI or CLI dry-run.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplatePreviewReport {
    pub rows: Vec<TemplatePreviewRow>,
    pub skipped: Vec<TemplateSkippedBlock>,
    pub warnings: Vec<TemplateBlockWarning>,
    pub parse_issues: Vec<IniParseIssue>,
    pub security: Vec<ImportedSecurity>,
    pub secret_choice_blocks: Vec<String>,
    pub total_blocks: usize,
    pub capped: bool,
    pub source_label: String,
    pub digest_sha256: String,
    pub plan_digest_sha256: String,
}

/// One profile applied during template import.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateAppliedRow {
    pub block_name: String,
    pub target_name: String,
    pub profile_id: String,
    pub protocol: String,
    pub action: TemplatePlannedAction,
    pub secret_disposition: SecretDisposition,
    pub secret_note: Option<String>,
    pub secret_form: Option<TemplateSecretForm>,
    pub has_stored_credential: bool,
}

/// Result of committing selected template profiles to the vault and active partition.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TemplateImportResult {
    pub applied: Vec<TemplateAppliedRow>,
    pub skipped_existing: Vec<String>,
    pub skipped_duplicate_in_template: Vec<String>,
    pub skipped_unselected: Vec<String>,
    pub skipped_invalid: Vec<TemplateSkippedBlock>,
    pub warnings: Vec<TemplateBlockWarning>,
    pub parse_issues: Vec<IniParseIssue>,
    pub security: Vec<ImportedSecurity>,
    pub oauth_apps_kept: Vec<String>,
    pub shared_secrets_failed: Vec<String>,
    pub stored: Vec<Value>,
    pub digest_sha256: String,
}

struct LoadedTemplate {
    content: String,
    base_dir: Option<PathBuf>,
    source_label: String,
    digest_sha256: String,
}

pub fn build_secret_policy(
    default_form: Option<TemplateSecretForm>,
    per_block: &HashMap<String, TemplateSecretForm>,
) -> TemplateSecretPolicy {
    let cleaned = per_block
        .iter()
        .filter_map(|(k, v)| {
            let trimmed = k.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some((trimmed.to_string(), *v))
            }
        })
        .collect();
    TemplateSecretPolicy {
        default_form,
        per_block: cleaned,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn load_template_input(input: &TemplateInput) -> Result<LoadedTemplate, String> {
    let cap = MAX_IMPORT_FILE_BYTES;
    let max_mb = cap / (1024 * 1024);
    match input {
        TemplateInput::Text { content } => {
            if content.len() as u64 > cap {
                return Err(format!(
                    "Configuration template exceeds the {} MB size limit.",
                    max_mb
                ));
            }
            let digest_sha256 = sha256_hex(content.as_bytes());
            Ok(LoadedTemplate {
                content: content.clone(),
                base_dir: None,
                source_label: "template".to_string(),
                digest_sha256,
            })
        }
        TemplateInput::File { path } => {
            let p = Path::new(path);
            let file = std::fs::File::open(p)
                .map_err(|e| format!("Cannot read configuration template file: {e}"))?;
            let meta = file
                .metadata()
                .map_err(|e| format!("Cannot inspect configuration template file: {e}"))?;
            if !meta.is_file() {
                return Err("Configuration template path is not a regular file.".to_string());
            }
            let mut bytes = Vec::new();
            file.take(cap + 1)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("Cannot read configuration template file: {e}"))?;
            if bytes.len() as u64 > cap {
                return Err(format!(
                    "Configuration template file exceeds the {} MB size limit.",
                    max_mb
                ));
            }
            let digest_sha256 = sha256_hex(&bytes);
            let content = String::from_utf8(bytes)
                .map_err(|_| "Configuration template file is not valid UTF-8.".to_string())?;
            let base_dir = p
                .canonicalize()
                .ok()
                .and_then(|resolved| resolved.parent().map(Path::to_path_buf))
                .or_else(|| p.parent().map(Path::to_path_buf));
            let source_label = p
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("template")
                .to_string();
            Ok(LoadedTemplate {
                content,
                base_dir,
                source_label,
                digest_sha256,
            })
        }
    }
}

fn server_has_any_secret(
    server: &ServerProfileExport,
    provider_secrets: &HashMap<String, ProviderSecrets>,
) -> bool {
    if server.credential.as_deref().is_some_and(|s| !s.is_empty()) {
        return true;
    }
    if provider_secrets.get(&server.id).is_some_and(|s| {
        s.oauth.is_some()
            || s.jotta_refresh.is_some()
            || s.proton_native_session.is_some()
            || s.filen_api_key.is_some()
            || s.aerocrypt_overlay_pw.is_some()
            || s.aerocrypt_overlay_salt.is_some()
    }) {
        return true;
    }
    server
        .options
        .as_ref()
        .and_then(|o| o.as_object())
        .is_some_and(|opts| {
            opts.get("rcloneCryptPassword")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
                || opts
                    .get("filen_api_key")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty())
        })
}

fn find_existing_by_trimmed_name<'a>(
    current: &'a [Value],
    trimmed_name: &str,
) -> Option<&'a Value> {
    current.iter().find(|p| {
        p.get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|n| n == trimmed_name)
    })
}

struct PlannedBlock {
    row: TemplatePreviewRow,
    security: Option<ImportedSecurity>,
    prepared_server: ServerProfileExport,
    existing_target_digest: Option<String>,
    reset_target_secrets: bool,
    duplicate_in_template: bool,
}

fn build_plan(
    eval: &TemplateEngineOutput,
    current: &[Value],
    existing_mode: ExistingProfileMode,
    relax_policy: &RelaxationPolicy,
) -> Vec<PlannedBlock> {
    let existing_name_strs: Vec<&str> = current
        .iter()
        .filter_map(|p| p.get("name").and_then(Value::as_str))
        .collect();
    let mut duplicate_names = ProfileNames::new(existing_name_strs.iter().copied());
    let mut skip_seen_trimmed: HashSet<String> = current
        .iter()
        .filter_map(|p| {
            p.get("name")
                .and_then(Value::as_str)
                .map(|s| s.trim().to_string())
        })
        .collect();
    let mut seen_in_template: HashSet<String> = HashSet::new();

    let mut out = Vec::with_capacity(eval.servers.len());
    for (file_index, raw_server) in eval.servers.iter().enumerate() {
        let mut server = raw_server.clone();
        let trimmed_name = server.name.trim().to_string();
        let start_line = eval
            .block_start_lines
            .get(&server.name)
            .or_else(|| eval.block_start_lines.get(&trimmed_name))
            .copied()
            .unwrap_or(1);
        let has_credential = server_has_any_secret(&server, &eval.provider_secrets);
        let secret_meta = eval
            .secret_blocks
            .get(&server.name)
            .or_else(|| eval.secret_blocks.get(&trimmed_name));
        let has_secret_input = secret_meta.is_some();
        let looks_obscured = secret_meta.is_some_and(|m| m.looks_obscured);
        let secret_form = secret_meta.and_then(|m| m.chosen_form);
        let needs_secret_form_choice = has_secret_input && secret_form.is_none();

        let matched = find_existing_by_trimmed_name(current, &trimmed_name);
        let existing_target_digest = matched.map(existing_target_digest_projection);
        let existing_match_id = matched
            .and_then(|p| p.get("id").and_then(Value::as_str))
            .map(str::to_string);
        let existing_match_name = matched
            .and_then(|p| p.get("name").and_then(Value::as_str))
            .map(str::to_string);

        let first_in_template = seen_in_template.insert(trimmed_name.clone());
        let (action, target_name) = match existing_mode {
            ExistingProfileMode::Skip => {
                if skip_seen_trimmed.contains(&trimmed_name) {
                    (
                        TemplatePlannedAction::Skip,
                        existing_match_name
                            .clone()
                            .unwrap_or_else(|| trimmed_name.clone()),
                    )
                } else {
                    skip_seen_trimmed.insert(trimmed_name.clone());
                    (TemplatePlannedAction::Add, trimmed_name.clone())
                }
            }
            ExistingProfileMode::Duplicate => {
                let claimed = duplicate_names.claim(&trimmed_name);
                let act = if claimed != trimmed_name {
                    TemplatePlannedAction::Rename
                } else {
                    TemplatePlannedAction::Add
                };
                (act, claimed)
            }
            ExistingProfileMode::Overwrite => {
                if matched.is_some() {
                    (TemplatePlannedAction::Replace, trimmed_name.clone())
                } else {
                    let claimed = duplicate_names.claim(&trimmed_name);
                    let act = if claimed != trimmed_name {
                        TemplatePlannedAction::Rename
                    } else {
                        TemplatePlannedAction::Add
                    };
                    (act, claimed)
                }
            }
        };

        let mut security = None;
        if action != TemplatePlannedAction::Skip {
            let mut relaxations = security_relaxations_of(
                &server.host,
                u16::try_from(server.port).ok(),
                server.provider_id.as_deref(),
                server.protocol.as_deref(),
                server.options.as_ref(),
            );
            if action == TemplatePlannedAction::Replace {
                if let Some(existing) = matched {
                    relaxations = new_relaxations(relaxations, existing);
                }
            }
            if !relaxations.is_empty() {
                let keep = relax_policy.kept_keys(file_index, &relaxations);
                let removed =
                    make_parts_safe(&mut server.host, &mut server.options, &relaxations, &keep);
                let (reset, kept): (Vec<_>, Vec<_>) = relaxations
                    .into_iter()
                    .partition(|r| removed.contains(&r.key));
                security = Some(ImportedSecurity {
                    file_index,
                    name: crate::profile_import_plan::display_name(&target_name, &server.host),
                    reset,
                    kept,
                });
            }
        }

        // Compute target credential disposition AFTER `make_parts_safe` so target identity
        // equality inspects the sanitized profile fields via `profile_import_plan::same_target`.
        let duplicate_in_template = action == TemplatePlannedAction::Skip && !first_in_template;
        let same_as_existing = action == TemplatePlannedAction::Replace
            && matched.is_some_and(|existing| {
                let incoming_value = serde_json::to_value(&server).unwrap_or(Value::Null);
                same_target(existing, &incoming_value)
            });
        let reset_target_secrets =
            action == TemplatePlannedAction::Replace && matched.is_some() && !same_as_existing;

        let (secret_disposition, secret_note) = match action {
            TemplatePlannedAction::Skip => {
                let note = if duplicate_in_template {
                    "duplicate in template"
                } else {
                    "skipped existing"
                };
                (SecretDisposition::None, Some(note.to_string()))
            }
            TemplatePlannedAction::Replace => {
                if matched.is_some() {
                    if server.credential.is_some() {
                        (
                            SecretDisposition::NewCredential,
                            Some("new credential".to_string()),
                        )
                    } else if needs_secret_form_choice {
                        (
                            SecretDisposition::None,
                            Some("password form not selected".to_string()),
                        )
                    } else if same_as_existing {
                        (
                            SecretDisposition::KeptExisting,
                            Some("existing password kept".to_string()),
                        )
                    } else {
                        (
                            SecretDisposition::ClearedExisting,
                            Some("existing password cleared".to_string()),
                        )
                    }
                } else {
                    (SecretDisposition::None, None)
                }
            }
            TemplatePlannedAction::Add | TemplatePlannedAction::Rename => {
                if has_credential {
                    (
                        SecretDisposition::NewCredential,
                        Some("new credential".to_string()),
                    )
                } else if needs_secret_form_choice {
                    (
                        SecretDisposition::None,
                        Some("password form not selected".to_string()),
                    )
                } else {
                    (SecretDisposition::None, None)
                }
            }
        };

        let protocol = server.protocol.clone().unwrap_or_default();
        let row = TemplatePreviewRow {
            file_index,
            block_name: server.name.clone(),
            target_name,
            start_line,
            protocol,
            provider_id: server.provider_id.clone(),
            host: server.host.clone(),
            port: server.port,
            username: server.username.clone(),
            has_credential,
            has_secret_input,
            looks_obscured,
            secret_form,
            needs_secret_form_choice,
            existing_match_id,
            existing_match_name,
            action,
            duplicate_in_template,
            secret_disposition,
            secret_note,
        };
        out.push(PlannedBlock {
            row,
            security,
            prepared_server: server,
            existing_target_digest,
            reset_target_secrets,
            duplicate_in_template,
        });
    }
    out
}

fn write_canonical_json(value: &Value, out: &mut Vec<u8>) {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => out.extend_from_slice(n.to_string().as_bytes()),
        Value::String(s) => {
            if let Ok(encoded) = serde_json::to_vec(s) {
                out.extend_from_slice(&encoded);
            }
        }
        Value::Array(items) => {
            out.push(b'[');
            for (idx, item) in items.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                write_canonical_json(item, out);
            }
            out.push(b']');
        }
        Value::Object(map) => {
            out.push(b'{');
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (idx, (k, v)) in entries.iter().enumerate() {
                if idx > 0 {
                    out.push(b',');
                }
                if let Ok(encoded_key) = serde_json::to_vec(k) {
                    out.extend_from_slice(&encoded_key);
                }
                out.push(b':');
                write_canonical_json(v, out);
            }
            out.push(b'}');
        }
    }
}

fn canonical_json_sha256(value: &Value) -> String {
    let mut buf = Vec::new();
    write_canonical_json(value, &mut buf);
    sha256_hex(&buf)
}

/// Digest ONLY the fields of an existing matched profile that template apply
/// acts on (identity, target endpoint, initialPath, options, security relaxations,
/// and stored-credential flag), ignoring volatile UI/telemetry metadata such as
/// `lastConnected`, quota caches, favicons, and connection-failure markers.
fn existing_target_digest_projection(matched: &Value) -> String {
    let mut relaxations: Vec<String> = crate::profile_security::security_relaxations(matched)
        .into_iter()
        .map(|r| format!("{}={}", r.key, r.value))
        .collect();
    relaxations.sort();
    relaxations.dedup();
    let projected = serde_json::json!({
        "id": matched.get("id").unwrap_or(&Value::Null),
        "name": matched.get("name").unwrap_or(&Value::Null),
        "protocol": matched.get("protocol").unwrap_or(&Value::Null),
        "host": matched.get("host").unwrap_or(&Value::Null),
        "port": matched.get("port").unwrap_or(&Value::Null),
        "username": matched.get("username").unwrap_or(&Value::Null),
        "providerId": matched.get("providerId").unwrap_or(&Value::Null),
        "initialPath": matched.get("initialPath").unwrap_or(&Value::Null),
        "hasStoredCredential": matched.get("hasStoredCredential").unwrap_or(&Value::Null),
        "options": matched.get("options").unwrap_or(&Value::Null),
        "securityRelaxations": relaxations,
    });
    canonical_json_sha256(&projected)
}

fn compute_plan_digest_sha256(
    existing_mode: ExistingProfileMode,
    planned: &[PlannedBlock],
) -> String {
    let rows_summary: Vec<Value> = planned
        .iter()
        .map(|entry| {
            let mut relaxation_reset_keys: Vec<String> = entry
                .security
                .iter()
                .flat_map(|s| s.reset.iter())
                .map(|r| r.key.to_string())
                .collect();
            relaxation_reset_keys.sort();
            relaxation_reset_keys.dedup();
            let mut relaxation_kept_keys: Vec<String> = entry
                .security
                .iter()
                .flat_map(|s| s.kept.iter())
                .map(|r| r.key.to_string())
                .collect();
            relaxation_kept_keys.sort();
            relaxation_kept_keys.dedup();
            let target_options_digest = entry
                .prepared_server
                .options
                .as_ref()
                .map(canonical_json_sha256);
            serde_json::json!({
                "blockName": entry.row.block_name,
                "targetName": entry.row.target_name,
                "protocol": entry.row.protocol,
                "providerId": entry.row.provider_id,
                "host": entry.row.host,
                "port": entry.row.port,
                "username": entry.row.username,
                "action": entry.row.action,
                "existingMatchId": entry.row.existing_match_id,
                "existingMatchName": entry.row.existing_match_name,
                "existingTargetDigest": entry.existing_target_digest,
                "targetInitialPath": entry.prepared_server.initial_path,
                "targetOptionsDigest": target_options_digest,
                "secretDisposition": entry.row.secret_disposition,
                "secretForm": entry.row.secret_form,
                "needsSecretFormChoice": entry.row.needs_secret_form_choice,
                "resetTargetSecrets": entry.reset_target_secrets,
                "relaxationResetKeys": relaxation_reset_keys,
                "relaxationKeptKeys": relaxation_kept_keys,
            })
        })
        .collect();
    let payload = serde_json::json!({
        "existing": existing_mode,
        "rows": rows_summary,
    });
    canonical_json_sha256(&payload)
}

/// Preview what a configuration template would import against `current` without
/// touching the vault or profile partition.
pub fn preview_template_with_current(
    input: &TemplateInput,
    options: &TemplatePreviewOptions,
    current: &[Value],
) -> Result<TemplatePreviewReport, String> {
    let loaded = load_template_input(input)?;
    let secret_policy = build_secret_policy(options.default_secret_form, &options.secret_forms);
    let eval = evaluate_template_text(
        &loaded.content,
        loaded.base_dir.as_deref(),
        &loaded.source_label,
        &secret_policy,
    )?;
    let planned = build_plan(
        &eval,
        current,
        options.existing,
        &RelaxationPolicy::ResetAll,
    );
    let plan_digest_sha256 = compute_plan_digest_sha256(options.existing, &planned);

    let mut secret_choice_blocks: Vec<String> = planned
        .iter()
        .filter(|entry| {
            entry.row.action != TemplatePlannedAction::Skip && entry.row.needs_secret_form_choice
        })
        .map(|entry| entry.row.block_name.clone())
        .collect();
    secret_choice_blocks.sort();
    secret_choice_blocks.dedup();

    let mut security = Vec::new();
    let mut rows = Vec::with_capacity(planned.len());
    for entry in planned {
        if let Some(sec) = entry.security {
            security.push(sec);
        }
        rows.push(entry.row);
    }

    Ok(TemplatePreviewReport {
        rows,
        skipped: eval.skipped,
        warnings: eval.warnings,
        parse_issues: eval.parse_issues,
        security,
        secret_choice_blocks,
        total_blocks: eval.total_blocks,
        capped: eval.capped,
        source_label: eval.source_label,
        digest_sha256: loaded.digest_sha256,
        plan_digest_sha256,
    })
}

struct AppliedMeta {
    action: TemplatePlannedAction,
    secret_disposition: SecretDisposition,
    secret_note: Option<String>,
    secret_form: Option<TemplateSecretForm>,
    block_name: String,
    has_stored_credential: bool,
    reset_target_secrets: bool,
}

/// Apply selected profiles from a configuration template atomically to `store`
/// and `user`'s active partition.
pub fn apply_template_for_user(
    store: &CredentialStore,
    user: i64,
    input: &TemplateInput,
    options: &TemplateApplyOptions,
) -> Result<TemplateImportResult, String> {
    user_partitions::init_or_migrate_cli(store)
        .map_err(|e| format!("{}: {e}", crate::IMPORT_NEEDS_PARTITION))?;
    let lock_conn = user_partitions::open_or_init_cli()
        .map_err(|e| format!("{}: {e}", crate::IMPORT_NEEDS_PARTITION))?;
    let lock = user_partitions::ProfileListLock::acquire(&lock_conn)?;
    crate::refuse_import_without_file_locks(lock.as_ref())?;
    user_partitions::ensure_credential_user_unlocked(user)?;
    let slots = crate::VaultSecretSlots { store, user };
    apply_template_with_slots(store, user, &slots, input, options)
}

pub(crate) fn apply_template_with_slots<S: SecretSlots>(
    store: &CredentialStore,
    user: i64,
    slots: &S,
    input: &TemplateInput,
    options: &TemplateApplyOptions,
) -> Result<TemplateImportResult, String> {
    let loaded = load_template_input(input)?;
    if let Some(expected) = options.expected_digest_sha256.as_deref() {
        if loaded.digest_sha256 != expected {
            return Err(
                "Configuration template changed after preview; review the updated template before importing."
                    .to_string(),
            );
        }
    }

    let secret_policy = build_secret_policy(options.default_secret_form, &options.secret_forms);
    let relax_policy = RelaxationPolicy::Keep(options.keep_relaxed.clone());
    let eval = evaluate_template_text(
        &loaded.content,
        loaded.base_dir.as_deref(),
        &loaded.source_label,
        &secret_policy,
    )?;

    apply_eval_with_slots(
        store,
        user,
        slots,
        eval,
        loaded.digest_sha256,
        &relax_policy,
        options,
    )
}

fn apply_eval_with_slots<S: SecretSlots>(
    store: &CredentialStore,
    user: i64,
    slots: &S,
    eval: TemplateEngineOutput,
    digest_sha256: String,
    relax_policy: &RelaxationPolicy,
    options: &TemplateApplyOptions,
) -> Result<TemplateImportResult, String> {
    let selected_set: Option<HashSet<String>> = options.selected_blocks.as_ref().map(|items| {
        items
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    });

    let mut replanned = false;
    loop {
        let (current, revision) =
            user_partitions::cli_read_active_profiles_at_revision(store, user).map_err(|e| {
                if e.starts_with(user_partitions::ACCOUNT_CHANGED) {
                    e
                } else {
                    format!("{}: {e}", crate::IMPORT_NEEDS_PARTITION)
                }
            })?;

        if let Some(expected_plan) = options.expected_plan_digest_sha256.as_deref() {
            let baseline_planned = build_plan(
                &eval,
                &current,
                options.existing,
                &RelaxationPolicy::ResetAll,
            );
            let live_plan_digest = compute_plan_digest_sha256(options.existing, &baseline_planned);
            if live_plan_digest != expected_plan {
                return Err(
                    "Saved profiles changed after template preview; refresh the preview before importing."
                        .to_string(),
                );
            }
        }
        let planned = build_plan(&eval, &current, options.existing, relax_policy);

        let mut skipped_existing = Vec::new();
        let mut skipped_duplicate_in_template = Vec::new();
        let mut skipped_unselected = Vec::new();
        let mut security = Vec::new();
        let mut to_commit_servers: Vec<ServerProfileExport> = Vec::new();
        let mut remapped_secrets: HashMap<String, ProviderSecrets> = HashMap::new();
        let mut meta_rows: Vec<AppliedMeta> = Vec::new();
        let mut reset_target_ids: Vec<String> = Vec::new();

        for entry in planned {
            let trimmed_block = entry.row.block_name.trim().to_string();
            if entry.row.action == TemplatePlannedAction::Skip {
                if entry.duplicate_in_template {
                    skipped_duplicate_in_template.push(entry.row.block_name.clone());
                } else {
                    skipped_existing.push(entry.row.block_name.clone());
                }
                continue;
            }
            if let Some(ref sel) = selected_set {
                if !sel.contains(&entry.row.block_name) && !sel.contains(&trimmed_block) {
                    skipped_unselected.push(entry.row.block_name.clone());
                    continue;
                }
            }
            if entry.row.needs_secret_form_choice {
                return Err(format!(
                    "Configuration template block '{}' includes a secret without an explicit secret form (clear or obscured).",
                    entry.row.block_name
                ));
            }
            if let Some(sec) = entry.security {
                security.push(sec);
            }

            let mut server = entry.prepared_server;
            let orig_id = server.id.clone();
            let final_id = if entry.row.action == TemplatePlannedAction::Replace {
                entry
                    .row
                    .existing_match_id
                    .clone()
                    .unwrap_or_else(|| orig_id.clone())
            } else {
                orig_id.clone()
            };
            server.id = final_id.clone();
            server.name = entry.row.target_name.clone();

            if let Some(secrets) = eval.provider_secrets.get(&orig_id) {
                remapped_secrets.insert(final_id.clone(), secrets.clone());
            }

            let existing_had_cred = entry
                .row
                .existing_match_id
                .as_deref()
                .and_then(|id| {
                    current
                        .iter()
                        .find(|p| p.get("id").and_then(Value::as_str) == Some(id))
                })
                .and_then(|p| p.get("hasStoredCredential").and_then(Value::as_bool))
                .unwrap_or(false);

            let stored_cred_after = match entry.row.secret_disposition {
                SecretDisposition::NewCredential => server.credential.is_some(),
                SecretDisposition::KeptExisting => existing_had_cred,
                SecretDisposition::ClearedExisting | SecretDisposition::None => false,
            };
            if entry.reset_target_secrets {
                reset_target_ids.push(final_id.clone());
            }

            meta_rows.push(AppliedMeta {
                action: entry.row.action,
                secret_disposition: entry.row.secret_disposition,
                secret_note: entry.row.secret_note.clone(),
                secret_form: entry.row.secret_form,
                block_name: entry.row.block_name.clone(),
                has_stored_credential: stored_cred_after,
                reset_target_secrets: entry.reset_target_secrets,
            });
            to_commit_servers.push(server);
        }

        if to_commit_servers.is_empty() {
            return Ok(TemplateImportResult {
                applied: Vec::new(),
                skipped_existing,
                skipped_duplicate_in_template,
                skipped_unselected,
                skipped_invalid: eval.skipped.clone(),
                warnings: eval.warnings.clone(),
                parse_issues: eval.parse_issues.clone(),
                security,
                oauth_apps_kept: Vec::new(),
                shared_secrets_failed: Vec::new(),
                stored: current,
                digest_sha256,
            });
        }

        let own_jotta_chains =
            crate::own_jotta_chains(store, &to_commit_servers, &remapped_secrets);
        let mut writes = crate::import_secret_writes(&to_commit_servers, &remapped_secrets);
        let restored = crate::restored_import_secrets(&to_commit_servers, &remapped_secrets);

        let mut overlay_writes: Vec<SecretWrite> = Vec::new();
        let mut commit_profiles: Vec<Value> = Vec::with_capacity(to_commit_servers.len());
        for (server, meta) in to_commit_servers.iter().zip(meta_rows.iter()) {
            let mut profile = crate::redact_imported_server(server, &restored);
            crate::bridge_commands::materialize_imported_crypt_overlay(
                &mut profile,
                &mut |key: &str, secret: &str| {
                    overlay_writes.push(SecretWrite::new(key.to_string(), None, secret));
                    Ok(())
                },
            )?;
            crate::bridge_commands::move_imported_filen_api_key(
                &mut profile,
                &mut |key: &str, secret: &str| {
                    overlay_writes.push(SecretWrite::new(key.to_string(), None, secret));
                    Ok(())
                },
            )?;
            profile["hasStoredCredential"] = Value::Bool(meta.has_stored_credential);
            if meta.action == TemplatePlannedAction::Replace {
                profile["importOutcome"] = Value::String(
                    if meta.reset_target_secrets {
                        "replaced_target"
                    } else {
                        "overwritten"
                    }
                    .to_string(),
                );
            }
            commit_profiles.push(profile);
        }
        writes.extend(overlay_writes);
        let preserve_keys: Vec<String> = writes.iter().map(|w| w.key.clone()).collect();
        for id in &reset_target_ids {
            for key in crate::per_profile_vault_keys_for_target_reset(id, &preserve_keys) {
                writes.push(SecretWrite::delete(key, None));
            }
        }

        let committed = write_secrets_and_commit(slots, writes, !replanned, || {
            user_partitions::cli_update_profiles_at_revision(store, user, &revision, |list| {
                lay_import_over(list, &commit_profiles);
            })
        })?;

        let (_sent, stored_list) = match committed {
            SecretCommit::Done(written) => written,
            SecretCommit::Replan => {
                replanned = true;
                continue;
            }
        };

        if let Err(e) = user_partitions::mirror_active_profiles_to_legacy_blob(store, &stored_list)
        {
            tracing::warn!("Could not mirror server profiles to the legacy blob: {e}");
        }

        let shared = crate::restore_shared_import_secrets(
            store,
            user,
            &to_commit_servers,
            &remapped_secrets,
            &own_jotta_chains,
        );

        let mut applied = Vec::with_capacity(to_commit_servers.len());
        let mut oauth_apps_kept = Vec::new();
        let mut shared_secrets_failed = Vec::new();
        for (server, meta) in to_commit_servers.iter().zip(meta_rows) {
            let target_name = stored_list
                .iter()
                .find(|p| p.get("id").and_then(Value::as_str) == Some(server.id.as_str()))
                .and_then(|p| p.get("name").and_then(Value::as_str))
                .unwrap_or(&server.name)
                .to_string();
            if shared.kept_oauth_apps.contains(&server.id) {
                oauth_apps_kept.push(target_name.clone());
            }
            if shared.failed.contains(&server.id) {
                shared_secrets_failed.push(target_name.clone());
            }
            applied.push(TemplateAppliedRow {
                block_name: meta.block_name,
                target_name,
                profile_id: server.id.clone(),
                protocol: server.protocol.clone().unwrap_or_default(),
                action: meta.action,
                secret_disposition: meta.secret_disposition,
                secret_note: meta.secret_note,
                secret_form: meta.secret_form,
                has_stored_credential: meta.has_stored_credential,
            });
        }

        tracing::info!(
            "Configuration template applied: {} imported, {} skipped, {} invalid",
            applied.len(),
            skipped_existing.len() + skipped_duplicate_in_template.len() + skipped_unselected.len(),
            eval.skipped.len()
        );

        return Ok(TemplateImportResult {
            applied,
            skipped_existing,
            skipped_duplicate_in_template,
            skipped_unselected,
            skipped_invalid: eval.skipped,
            warnings: eval.warnings,
            parse_issues: eval.parse_issues,
            security,
            oauth_apps_kept,
            shared_secrets_failed,
            stored: stored_list,
            digest_sha256,
        });
    }
}

/// Preview what a configuration template would import for the active user,
/// without writing any secrets or profile list entries.
#[tauri::command]
pub async fn preview_configuration_template(
    webview: tauri::Webview,
    input: TemplateInput,
    options: Option<TemplatePreviewOptions>,
) -> Result<TemplatePreviewReport, String> {
    crate::only_main_window(webview.label(), "preview_configuration_template")?;
    tokio::task::spawn_blocking(move || {
        let store = CredentialStore::from_cache().ok_or(crate::IMPORT_VAULT_NOT_READY)?;
        let user = user_partitions::cli_get_active_user(&store)
            .map_err(|e| format!("{}: {e}", crate::IMPORT_NEEDS_PARTITION))?
            .ok_or_else(|| format!("{}: NO_ACTIVE_USER", crate::IMPORT_NEEDS_PARTITION))?;
        let (current, _) = user_partitions::cli_read_active_profiles_at_revision(&store, user.id)
            .map_err(|e| format!("{}: {e}", crate::IMPORT_NEEDS_PARTITION))?;
        preview_template_with_current(&input, &options.unwrap_or_default(), &current)
    })
    .await
    .map_err(|e| format!("Template preview task failed: {e}"))?
}

/// Atomically store selected configuration template profiles and secrets for the
/// active user.
#[tauri::command]
pub async fn import_configuration_template(
    webview: tauri::Webview,
    input: TemplateInput,
    options: Option<TemplateApplyOptions>,
) -> Result<TemplateImportResult, String> {
    crate::only_main_window(webview.label(), "import_configuration_template")?;
    tokio::task::spawn_blocking(move || {
        let store = CredentialStore::from_cache().ok_or(crate::IMPORT_VAULT_NOT_READY)?;
        let user = user_partitions::cli_get_active_user(&store)
            .map_err(|e| format!("{}: {e}", crate::IMPORT_NEEDS_PARTITION))?
            .ok_or_else(|| format!("{}: NO_ACTIVE_USER", crate::IMPORT_NEEDS_PARTITION))?;
        apply_template_for_user(&store, user.id, &input, &options.unwrap_or_default())
    })
    .await
    .map_err(|e| format!("Template import task failed: {e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct TestVaultEnv {
        dir: TempDir,
        prev_data_root: Option<std::path::PathBuf>,
        prev_xdg: Option<std::ffi::OsString>,
        prev_pp: Option<std::ffi::OsString>,
        store: CredentialStore,
        user_id: i64,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl TestVaultEnv {
        fn new() -> Self {
            let guard = crate::test_env::lock();
            CredentialStore::lock();
            let dir = TempDir::new().unwrap();
            let prev_data_root =
                crate::portable::set_test_data_root(Some(dir.path().to_path_buf()));
            let prev_xdg = std::env::var_os("XDG_CONFIG_HOME");
            let prev_pp = std::env::var_os("AEROFTP_USER_PASSPHRASE");
            std::env::set_var("XDG_CONFIG_HOME", dir.path());
            std::env::remove_var("AEROFTP_USER_PASSPHRASE");

            CredentialStore::bootstrap_master_password("test-master-password-req219")
                .expect("bootstrap test vault");
            CredentialStore::unlock_with_master("test-master-password-req219", None)
                .expect("unlock test vault");
            let store = CredentialStore::from_cache().expect("cached test vault");
            user_partitions::init_or_migrate_cli(&store).unwrap();
            let user = user_partitions::cli_get_active_user(&store)
                .unwrap()
                .expect("default active user");
            user_partitions::ensure_credential_user_unlocked(user.id).unwrap();

            Self {
                dir,
                prev_data_root,
                prev_xdg,
                prev_pp,
                store,
                user_id: user.id,
                _guard: guard,
            }
        }
    }

    impl Drop for TestVaultEnv {
        fn drop(&mut self) {
            CredentialStore::lock();
            crate::portable::set_test_data_root(self.prev_data_root.take());
            match &self.prev_xdg {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
            match &self.prev_pp {
                Some(v) => std::env::set_var("AEROFTP_USER_PASSPHRASE", v),
                None => std::env::remove_var("AEROFTP_USER_PASSPHRASE"),
            }
        }
    }

    #[test]
    fn preview_default_requires_secret_form_choice_and_never_mentions_rclone() {
        let cfg = "\
[alpha]
type = sftp
host = alpha.example.test
user = alice
pass = PlainSecret123!

[broken]
type = nonexistent_backend
";
        let preview = preview_template_with_current(
            &TemplateInput::Text {
                content: cfg.to_string(),
            },
            &TemplatePreviewOptions::default(),
            &[],
        )
        .unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert!(preview.rows[0].has_secret_input);
        assert!(!preview.rows[0].looks_obscured);
        assert_eq!(preview.rows[0].secret_form, None);
        assert!(preview.rows[0].needs_secret_form_choice);
        assert!(!preview.rows[0].has_credential);
        assert_eq!(preview.secret_choice_blocks, vec!["alpha"]);
        assert_eq!(preview.rows[0].action, TemplatePlannedAction::Add);
        assert_eq!(preview.skipped.len(), 1);
        assert!(!serde_json::to_string(&preview)
            .unwrap()
            .to_lowercase()
            .contains("rclone"));
    }

    #[test]
    fn replace_without_secret_keeps_existing_password_when_identity_is_unchanged() {
        let env = TestVaultEnv::new();

        // Seed existing profile + stored secret in vault and partition.
        let seed_tpl = "\
[prod-sftp]
type = sftp
host = files.example.test
port = 2222
user = deploy
pass = InitialVaultPass!
";
        let seed = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: seed_tpl.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(seed.applied.len(), 1);
        let id = seed.applied[0].profile_id.clone();
        assert!(seed.applied[0].has_stored_credential);
        assert_eq!(seed.applied[0].secret_form, Some(TemplateSecretForm::Clear));
        assert_eq!(
            env.store.get(&format!("server_{id}")).unwrap(),
            "InitialVaultPass!"
        );

        // Overwrite with NO pass, keeping identical (protocol, host, port, username).
        let update_tpl = "\
[prod-sftp]
type = sftp
host = files.example.test
port = 2222
user = deploy
";
        let (current, _) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        let preview = preview_template_with_current(
            &TemplateInput::Text {
                content: update_tpl.to_string(),
            },
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
            &current,
        )
        .unwrap();
        assert_eq!(preview.rows[0].action, TemplatePlannedAction::Replace);
        assert_eq!(
            preview.rows[0].secret_disposition,
            SecretDisposition::KeptExisting
        );
        assert_eq!(
            preview.rows[0].secret_note.as_deref(),
            Some("existing password kept")
        );

        let res = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: update_tpl.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            res.applied[0].secret_disposition,
            SecretDisposition::KeptExisting
        );
        assert!(res.applied[0].has_stored_credential);
        assert_eq!(
            env.store.get(&format!("server_{id}")).unwrap(),
            "InitialVaultPass!"
        );
    }

    #[test]
    fn replace_without_secret_clears_existing_password_when_host_or_user_changes() {
        let env = TestVaultEnv::new();

        let seed_tpl = "\
[prod-sftp]
type = sftp
host = old.example.test
port = 22
user = alice
pass = OldServerSecret!
";
        let seed = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: seed_tpl.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let id = seed.applied[0].profile_id.clone();
        user_partitions::store_imported_credential_for_user(
            &env.store,
            env.user_id,
            &format!("server_modes_{id}"),
            None,
            r#"{"sftp":{"password":"x"}}"#,
        )
        .unwrap();
        assert_eq!(
            env.store.get(&format!("server_{id}")).unwrap(),
            "OldServerSecret!"
        );

        // Overwrite with DIFFERENT host and NO password -> must clear stored secret!
        let moved_tpl = "\
[prod-sftp]
type = sftp
host = new.example.test
port = 22
user = alice
";
        let (current, _) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        let preview = preview_template_with_current(
            &TemplateInput::Text {
                content: moved_tpl.to_string(),
            },
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
            &current,
        )
        .unwrap();
        assert_eq!(preview.rows[0].action, TemplatePlannedAction::Replace);
        assert_eq!(
            preview.rows[0].secret_disposition,
            SecretDisposition::ClearedExisting
        );
        assert_eq!(
            preview.rows[0].secret_note.as_deref(),
            Some("existing password cleared")
        );

        let res = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: moved_tpl.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            res.applied[0].secret_disposition,
            SecretDisposition::ClearedExisting
        );
        assert!(!res.applied[0].has_stored_credential);
        assert!(env.store.get(&format!("server_{id}")).is_err());
        assert!(env.store.get(&format!("server_modes_{id}")).is_err());
        let stored_profile = res
            .stored
            .iter()
            .find(|p| p.get("id").and_then(Value::as_str) == Some(id.as_str()))
            .unwrap();
        assert_eq!(
            stored_profile
                .get("hasStoredCredential")
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn duplicate_mode_renames_colliding_profiles_and_skip_mode_preserves_existing() {
        let env = TestVaultEnv::new();

        let first_tpl = "\
[team-dav]
type = webdav
url = https://dav1.example.test/remote.php/dav/files/alice
user = alice
";
        apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: first_tpl.to_string(),
            },
            &TemplateApplyOptions::default(),
        )
        .unwrap();

        let second_tpl = "\
[team-dav]
type = webdav
url = https://dav2.example.test/remote.php/dav/files/bob
user = bob
";
        let skipped = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: second_tpl.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Skip,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(skipped.applied.is_empty());
        assert_eq!(skipped.skipped_existing, vec!["team-dav"]);

        let renamed = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: second_tpl.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Duplicate,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(renamed.applied.len(), 1);
        assert_eq!(renamed.applied[0].action, TemplatePlannedAction::Rename);
        assert_eq!(renamed.applied[0].target_name, "team-dav (1)");
    }

    #[test]
    fn file_input_expands_key_file_relative_to_config_dir_and_checks_digest() {
        let env = TestVaultEnv::new();
        let key_path = env.dir.path().join("id_ed25519");
        std::fs::write(&key_path, "dummy-key").unwrap();
        let tpl_path = env.dir.path().join("servers.ini");
        std::fs::write(
            &tpl_path,
            "[bastion]\ntype = sftp\nhost = ssh.example.test\nuser = ops\nkey_file = $RCLONE_CONFIG_DIR/id_ed25519\n",
        )
        .unwrap();

        let file_input = TemplateInput::File {
            path: tpl_path.display().to_string(),
        };
        let preview =
            preview_template_with_current(&file_input, &TemplatePreviewOptions::default(), &[])
                .unwrap();
        let wrong_digest = apply_template_for_user(
            &env.store,
            env.user_id,
            &file_input,
            &TemplateApplyOptions {
                expected_digest_sha256: Some("0".repeat(64)),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(wrong_digest.contains("changed after preview"));

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &file_input,
            &TemplateApplyOptions {
                expected_digest_sha256: Some(preview.digest_sha256),
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap();
        let stored = &applied.stored[0];
        assert_eq!(
            stored
                .pointer("/options/private_key_path")
                .and_then(Value::as_str),
            Some(
                env.dir
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join("id_ed25519")
                    .to_string_lossy()
                    .as_ref()
            )
        );
    }

    #[test]
    fn template_security_relaxations_strip_unless_kept() {
        let env = TestVaultEnv::new();
        let tpl = "\
[insecure-ftp]
type = ftp
host = ftp.example.test
user = bob
pass = BobSecret!
no_check_certificate = true
";
        let preview = preview_template_with_current(
            &TemplateInput::Text {
                content: tpl.to_string(),
            },
            &TemplatePreviewOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
        assert_eq!(preview.security.len(), 1);
        assert!(!preview.security[0].reset.is_empty());
        assert!(preview.security[0].kept.is_empty());

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: tpl.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                keep_relaxed: Vec::new(),
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap();
        assert_ne!(
            applied.stored[0]
                .pointer("/options/acceptInvalidCerts")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn webdav_template_pasted_and_file_connect_to_url_base_path() {
        let env = TestVaultEnv::new();
        let pass = crate::rclone_import::obscure_password("ltc-secret").unwrap();
        let tpl = format!(
            "[ltc]\ntype = webdav\nurl = https://lifetimecloud.me/dav/alice/\nuser = alice\npass = {pass}\n"
        );
        let apply_opts = TemplateApplyOptions {
            default_secret_form: Some(TemplateSecretForm::Obscured),
            ..Default::default()
        };

        let pasted = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: tpl.clone(),
            },
            &apply_opts,
        )
        .unwrap();
        let stored_paste = &pasted.stored[0];
        let paste_host = stored_paste.get("host").and_then(Value::as_str).unwrap();
        let (paste_extra, paste_resolved_host) = crate::profile_loader::profile_connection_extra(
            stored_paste,
            crate::providers::ProviderType::WebDav,
            paste_host,
        );
        let paste_url = crate::providers::WebDavConfig::from_provider_config(
            &crate::providers::ProviderConfig {
                name: "ltc".to_string(),
                provider_type: crate::providers::ProviderType::WebDav,
                host: paste_resolved_host,
                port: Some(443),
                username: Some("alice".to_string()),
                password: Some("ltc-secret".to_string()),
                initial_path: None,
                extra: paste_extra,
            },
        )
        .unwrap()
        .url;
        assert_eq!(paste_url, "https://lifetimecloud.me/dav/alice/");
        drop(env);

        let env_file = TestVaultEnv::new();
        let tpl_path = env_file.dir.path().join("ltc.ini");
        std::fs::write(&tpl_path, &tpl).unwrap();
        let file_input = TemplateInput::File {
            path: tpl_path.display().to_string(),
        };
        let applied_file =
            apply_template_for_user(&env_file.store, env_file.user_id, &file_input, &apply_opts)
                .unwrap();
        let stored_file = &applied_file.stored[0];
        let file_host = stored_file.get("host").and_then(Value::as_str).unwrap();
        let (file_extra, file_resolved_host) = crate::profile_loader::profile_connection_extra(
            stored_file,
            crate::providers::ProviderType::WebDav,
            file_host,
        );
        let file_url = crate::providers::WebDavConfig::from_provider_config(
            &crate::providers::ProviderConfig {
                name: "ltc".to_string(),
                provider_type: crate::providers::ProviderType::WebDav,
                host: file_resolved_host,
                port: Some(443),
                username: Some("alice".to_string()),
                password: Some("ltc-secret".to_string()),
                initial_path: None,
                extra: file_extra,
            },
        )
        .unwrap()
        .url;
        assert_eq!(file_url, "https://lifetimecloud.me/dav/alice/");
    }

    #[test]
    fn plain_23char_alnum_requires_explicit_choice_and_never_auto_reveals() {
        let env = TestVaultEnv::new();
        let plain_23 = "AbcdefghijKlmnoPqrs1234";
        let tpl = format!(
            "[plain23]\ntype = sftp\nhost = box.example.test\nuser = alice\npass = {plain_23}\n"
        );
        let input = TemplateInput::Text { content: tpl };

        let preview =
            preview_template_with_current(&input, &TemplatePreviewOptions::default(), &[]).unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert!(preview.rows[0].has_secret_input);
        assert!(preview.rows[0].looks_obscured);
        assert_eq!(preview.rows[0].secret_form, None);
        assert!(preview.rows[0].needs_secret_form_choice);
        assert!(!preview.rows[0].has_credential);

        let unchosen = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions::default(),
        )
        .unwrap_err();
        assert!(unchosen.contains("explicit secret form"));

        let mut per_block = HashMap::new();
        per_block.insert("plain23".to_string(), TemplateSecretForm::Clear);
        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                secret_forms: per_block,
                ..Default::default()
            },
        )
        .unwrap();
        let id = &applied.applied[0].profile_id;
        assert_eq!(
            applied.applied[0].secret_form,
            Some(TemplateSecretForm::Clear)
        );
        assert_eq!(env.store.get(&format!("server_{id}")).unwrap(), plain_23);
    }

    #[test]
    fn overwrite_exercises_shared_same_target_vectors_after_make_parts_safe() {
        for (label, existing, incoming, expected_keep) in
            crate::profile_import_plan::same_target_shared_vectors()
        {
            let existing_name = existing
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("Target")
                .to_string();
            let mut existing_with_name = existing.clone();
            if let Some(obj) = existing_with_name.as_object_mut() {
                obj.entry("id")
                    .or_insert_with(|| Value::String("srv_1".to_string()));
                obj.entry("name")
                    .or_insert_with(|| Value::String(existing_name.clone()));
            }
            let incoming_server = ServerProfileExport {
                id: "tpl_1".to_string(),
                name: existing_name,
                host: incoming
                    .get("host")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                port: incoming.get("port").and_then(Value::as_u64).unwrap_or(0) as u32,
                username: incoming
                    .get("username")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                protocol: incoming
                    .get("protocol")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                provider_id: incoming
                    .get("providerId")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                options: incoming.get("options").cloned(),
                ..Default::default()
            };
            let relaxations = security_relaxations_of(
                &incoming_server.host,
                u16::try_from(incoming_server.port).ok(),
                incoming_server.provider_id.as_deref(),
                incoming_server.protocol.as_deref(),
                incoming_server.options.as_ref(),
            );
            let keep: Vec<KeptRelaxation> = relaxations
                .iter()
                .map(|r| KeptRelaxation {
                    file_index: 0,
                    key: r.key.to_string(),
                })
                .collect();
            let eval = TemplateEngineOutput {
                servers: vec![incoming_server],
                provider_secrets: HashMap::new(),
                block_start_lines: HashMap::new(),
                secret_blocks: HashMap::new(),
                skipped: Vec::new(),
                warnings: Vec::new(),
                parse_issues: Vec::new(),
                total_blocks: 1,
                capped: false,
                source_label: "test".to_string(),
            };
            let planned = build_plan(
                &eval,
                &[existing_with_name],
                ExistingProfileMode::Overwrite,
                &RelaxationPolicy::Keep(keep),
            );
            let expected_disp = if expected_keep {
                SecretDisposition::KeptExisting
            } else {
                SecretDisposition::ClearedExisting
            };
            assert_eq!(
                planned[0].row.secret_disposition, expected_disp,
                "shared vector failed: {label}"
            );
        }

        // Also confirm via real INI text that resetting a relaxation before same_target
        // keeps the existing credential because the stored profile remains strict.
        let env_reset_existing = serde_json::json!({
            "id": "srv_dav",
            "name": "WebDAV",
            "protocol": "webdav",
            "providerId": "custom-webdav",
            "host": "dav.example.test",
            "port": 443,
            "username": "alice",
            "options": { "webdavScheme": "https" }
        });
        let tpl = "\
[WebDAV]
type = webdav
url = https://dav.example.test/
user = alice
no_check_certificate = true
";
        let preview = preview_template_with_current(
            &TemplateInput::Text {
                content: tpl.to_string(),
            },
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
            &[env_reset_existing],
        )
        .unwrap();
        assert_eq!(
            preview.rows[0].secret_disposition,
            SecretDisposition::KeptExisting
        );
    }

    struct FailingDeleteSlots<'a> {
        inner: crate::VaultSecretSlots<'a>,
    }

    impl SecretSlots for FailingDeleteSlots<'_> {
        type Snapshot = user_partitions::CredentialSnapshot;

        fn snapshot(&self, write: &SecretWrite) -> Result<Self::Snapshot, String> {
            self.inner.snapshot(write)
        }

        fn write(&self, write: &SecretWrite) -> Result<(), String> {
            if write.delete {
                return Err(format!("simulated delete error for {}", write.key));
            }
            self.inner.write(write)
        }

        fn restore(&self, write: &SecretWrite, before: Self::Snapshot) -> Result<(), String> {
            self.inner.restore(write, before)
        }
    }

    #[test]
    fn failing_credential_delete_aborts_commit_without_touching_partition() {
        let env = TestVaultEnv::new();
        let seed_tpl = "\
[prod-sftp]
type = sftp
host = old.example.test
port = 22
user = alice
pass = InitialSecret!
";
        let seed = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: seed_tpl.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let id = seed.applied[0].profile_id.clone();
        user_partitions::store_imported_credential_for_user(
            &env.store,
            env.user_id,
            &format!("server_modes_{id}"),
            None,
            r#"{"sftp":{"password":"x"}}"#,
        )
        .unwrap();

        let moved_tpl = "\
[prod-sftp]
type = sftp
host = new.example.test
port = 22
user = alice
";
        let failing_slots = FailingDeleteSlots {
            inner: crate::VaultSecretSlots {
                store: &env.store,
                user: env.user_id,
            },
        };
        let err = apply_template_with_slots(
            &env.store,
            env.user_id,
            &failing_slots,
            &TemplateInput::Text {
                content: moved_tpl.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("could not be cleared"), "{err}");

        let (current, _) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(
            current[0].get("host").and_then(Value::as_str),
            Some("old.example.test")
        );
        assert_eq!(
            current[0]
                .get("hasStoredCredential")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            env.store.get(&format!("server_{id}")).unwrap(),
            "InitialSecret!"
        );
        assert!(env.store.get(&format!("server_modes_{id}")).is_ok());
    }

    #[test]
    fn webdav_url_userinfo_is_stripped_from_preview_and_stored_profile() {
        let env = TestVaultEnv::new();
        let tpl = "\
[davauth]
type = webdav
url = https://alice:SuperSecret!@dav.example.test/dav/folder/
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };

        let preview =
            preview_template_with_current(&input, &TemplatePreviewOptions::default(), &[]).unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.rows[0].host, "dav.example.test");
        assert_eq!(preview.rows[0].username, "alice");
        assert!(!preview.rows[0].has_secret_input);
        assert!(!preview.rows[0].needs_secret_form_choice);
        assert!(preview.rows[0].has_credential);
        assert_eq!(
            preview.rows[0].secret_disposition,
            SecretDisposition::NewCredential
        );
        let preview_json = serde_json::to_string(&preview).unwrap();
        assert!(!preview_json.contains("SuperSecret"));
        assert!(!preview_json.contains("alice:"));

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions::default(),
        )
        .unwrap();
        let stored = &applied.stored[0];
        assert_eq!(
            stored.get("host").and_then(Value::as_str),
            Some("dav.example.test")
        );
        assert_eq!(
            stored.get("username").and_then(Value::as_str),
            Some("alice")
        );
        assert_eq!(
            stored.pointer("/options/basePath").and_then(Value::as_str),
            Some("/dav/folder/")
        );
        assert!(!serde_json::to_string(stored)
            .unwrap()
            .contains("SuperSecret"));
        let id = &applied.applied[0].profile_id;
        assert_eq!(
            env.store.get(&format!("server_{id}")).unwrap(),
            "SuperSecret!"
        );

        // RFC 3986 percent-encoded userinfo must decode both user and
        // password while preserving invalid escapes verbatim.
        let encoded_tpl = "\
[davenc]
type = webdav
url = https://alice%40corp:p%40ss@dav.example.test/dav/folder/

[davbadpct]
type = webdav
url = https://bad%ZZusr:p%40ss%ZZ@dav.example.test/dav/sub/
";
        let encoded_input = TemplateInput::Text {
            content: encoded_tpl.to_string(),
        };
        let enc_preview =
            preview_template_with_current(&encoded_input, &TemplatePreviewOptions::default(), &[])
                .unwrap();
        assert_eq!(enc_preview.rows.len(), 2);
        assert_eq!(enc_preview.rows[0].host, "dav.example.test");
        assert_eq!(enc_preview.rows[0].username, "alice@corp");
        assert_eq!(enc_preview.rows[1].host, "dav.example.test");
        assert_eq!(enc_preview.rows[1].username, "bad%ZZusr");
        let enc_preview_json = serde_json::to_string(&enc_preview).unwrap();
        assert!(!enc_preview_json.contains("p@ss"));
        assert!(!enc_preview_json.contains("p%40ss"));

        let enc_applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &encoded_input,
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(enc_applied.applied.len(), 2);
        let stored_enc = &enc_applied.stored[1];
        assert_eq!(
            stored_enc.get("username").and_then(Value::as_str),
            Some("alice@corp")
        );
        assert_eq!(
            stored_enc.get("host").and_then(Value::as_str),
            Some("dav.example.test")
        );
        let id_enc = &enc_applied.applied[0].profile_id;
        assert_eq!(env.store.get(&format!("server_{id_enc}")).unwrap(), "p@ss");
        let stored_badpct = &enc_applied.stored[2];
        assert_eq!(
            stored_badpct.get("username").and_then(Value::as_str),
            Some("bad%ZZusr")
        );
        let id_badpct = &enc_applied.applied[1].profile_id;
        assert_eq!(
            env.store.get(&format!("server_{id_badpct}")).unwrap(),
            "p@ss%ZZ"
        );
    }

    #[test]
    fn refuses_template_block_when_host_key_carries_userinfo() {
        let env = TestVaultEnv::new();
        let tpl = "\
[ftpuinfo]
type = ftp
host = ftp://alice:FtpSecret!@ftp.example.test

[sftpuinfo]
type = sftp
host = bob:SftpSecret!@sftp.example.test
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let preview =
            preview_template_with_current(&input, &TemplatePreviewOptions::default(), &[]).unwrap();
        assert!(
            preview.rows.is_empty(),
            "blocks with userinfo in host must be refused: {:?}",
            preview.rows
        );
        assert_eq!(preview.skipped.len(), 2);
        let preview_json = serde_json::to_string(&preview).unwrap();
        assert!(!preview_json.contains("FtpSecret"));
        assert!(!preview_json.contains("SftpSecret"));

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(applied.applied.is_empty());
        assert_eq!(applied.skipped_invalid.len(), 2);
        assert!(applied.stored.is_empty());
    }

    #[test]
    fn refuses_template_block_when_endpoint_key_carries_userinfo() {
        let env = TestVaultEnv::new();
        let tpl = "\
[s3uinfo]
type = s3
provider = Other
endpoint = https://AKIA:S3Secret!@s3.example.test/bucket

[s3pathat]
type = s3
provider = Other
endpoint = https://s3.example.test/@public
access_key_id = AKIAOK
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let preview =
            preview_template_with_current(&input, &TemplatePreviewOptions::default(), &[]).unwrap();
        assert_eq!(preview.rows.len(), 1);
        assert_eq!(preview.rows[0].block_name, "s3pathat");
        assert_eq!(preview.skipped.len(), 1);
        assert_eq!(preview.skipped[0].name, "s3uinfo");
        let preview_json = serde_json::to_string(&preview).unwrap();
        assert!(!preview_json.contains("S3Secret"));

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(applied.applied.len(), 1);
        assert_eq!(applied.applied[0].target_name, "s3pathat");
        assert_eq!(applied.skipped_invalid.len(), 1);
        assert_eq!(applied.skipped_invalid[0].name, "s3uinfo");
    }

    #[test]
    fn apply_refuses_when_saved_profiles_drift_between_preview_and_apply() {
        let env = TestVaultEnv::new();
        let tpl = "\
[drift]
type = sftp
host = sftp.example.test
user = bob
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let preview = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Duplicate,
                ..Default::default()
            },
            &[],
        )
        .unwrap();

        // Simulate another tab/command adding a profile with the same name before apply runs.
        apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions::default(),
        )
        .unwrap();

        let err = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Duplicate,
                expected_digest_sha256: Some(preview.digest_sha256),
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("Saved profiles changed after template preview"));
    }

    #[test]
    fn replace_on_changed_target_clears_all_old_vault_keys_and_stored_secret_metadata() {
        let env = TestVaultEnv::new();
        let initial_id = "srv_old_target".to_string();
        let seeded_profile = serde_json::json!({
            "id": initial_id,
            "name": "target-reset",
            "protocol": "googledrive",
            "providerId": "googledrive",
            "host": "www.googleapis.com",
            "port": 443,
            "username": "alice@example.com",
            "color": "#224466",
            "localInitialPath": "/tmp/work",
            "siteTemplates": [{ "id": "t1", "name": "Pwd", "line": "PWD" }],
            "customIconUrl": "data:image/png;base64,iVBORw0KGgo=",
            "faviconUrl": "https://www.googleapis.com/favicon.ico",
            "publicUrlBase": "https://drive.example.test/share/",
            "skipDeltaEligibilityPrompt": true,
            "lastConnected": "2026-11-14T00:00:00Z",
            "lastQuota": { "used": 10, "total": 100, "fetched_at": "2026-11-14T00:00:00Z" },
            "lastBucketEncryption": { "enabled": true },
            "lastCompression": { "ratio": 0.7 },
            "lastConnectionError": "err",
            "deviceFingerprint": { "serial": "s1" },
            "hasStoredCredential": true,
            "hasStoredFilenApiKey": true,
            "hasStoredAeroCryptPassword": true,
            "hasStoredAeroCryptSalt": true,
            "hasStoredAeroCryptKeyfilePath": true,
            "hasStoredAeroCryptConfig": true,
            "aeroCryptOverlay": { "enabled": true, "salt": "abcd" },
            "persistModeCredentials": true,
            "options": {
                "sasToken": "?sr=c&sig=secret",
                "private_key_path": "/home/alice/.ssh/id_old",
                "key_passphrase": "old-key-passphrase",
            }
        });
        let (_, rev) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        let seeded_clone = seeded_profile.clone();
        user_partitions::cli_update_profiles_at_revision(&env.store, env.user_id, &rev, |list| {
            list.push(seeded_clone);
        })
        .unwrap();
        let old_keys = crate::per_profile_vault_keys_for_delete(&initial_id, Some("googledrive"));
        for key in &old_keys {
            user_partitions::store_imported_credential_for_user(
                &env.store,
                env.user_id,
                key,
                None,
                &format!("secret-for-{key}"),
            )
            .unwrap();
        }

        // Case A: Replace pointing to another host without password -> clears ALL old keys and flags.
        let replace_no_pass = "\
[target-reset]
type = webdav
url = https://new.example.test/dav/
user = bob
";
        let res_no_pass = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: replace_no_pass.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res_no_pass.applied.len(), 1);
        assert_eq!(res_no_pass.applied[0].profile_id, initial_id);
        let stored_a = &res_no_pass.stored[0];
        assert_eq!(
            stored_a.get("hasStoredCredential").and_then(Value::as_bool),
            Some(false)
        );
        assert!(stored_a.get("hasStoredFilenApiKey").is_none());
        assert!(stored_a.get("hasStoredAeroCryptPassword").is_none());
        assert!(stored_a.get("hasStoredAeroCryptSalt").is_none());
        assert!(stored_a.get("hasStoredAeroCryptKeyfilePath").is_none());
        assert!(stored_a.get("hasStoredAeroCryptConfig").is_none());
        assert!(stored_a.get("aeroCryptOverlay").is_none());
        assert!(stored_a.get("persistModeCredentials").is_none());
        assert!(stored_a.pointer("/options/sasToken").is_none());
        assert!(stored_a.pointer("/options/private_key_path").is_none());
        assert!(stored_a.get("lastQuota").is_none());
        assert!(stored_a.get("lastBucketEncryption").is_none());
        assert!(stored_a.get("lastCompression").is_none());
        assert!(stored_a.get("lastConnectionError").is_none());
        assert!(stored_a.get("deviceFingerprint").is_none());
        assert!(stored_a.get("faviconUrl").is_none());
        assert!(stored_a.get("publicUrlBase").is_none());
        assert!(stored_a.get("skipDeltaEligibilityPrompt").is_none());
        assert!(stored_a.get("lastConnected").is_none_or(Value::is_null));
        assert_eq!(
            stored_a.get("color").and_then(Value::as_str),
            Some("#224466")
        );
        assert_eq!(
            stored_a.get("localInitialPath").and_then(Value::as_str),
            Some("/tmp/work")
        );
        assert_eq!(
            stored_a
                .get("siteTemplates")
                .and_then(Value::as_array)
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            stored_a.get("customIconUrl").and_then(Value::as_str),
            Some("data:image/png;base64,iVBORw0KGgo=")
        );
        assert!(stored_a.pointer("/options/key_passphrase").is_none());
        for key in &old_keys {
            assert!(
                env.store.get(key).is_err(),
                "stale key {key} must be deleted when target changed"
            );
        }

        // Re-seed all old keys on srv_old_target, then replace onto a NEW target WITH a new password.
        for key in &old_keys {
            user_partitions::store_imported_credential_for_user(
                &env.store,
                env.user_id,
                key,
                None,
                &format!("stale-for-{key}"),
            )
            .unwrap();
        }
        let replace_with_pass = "\
[target-reset]
type = sftp
host = ssh.other.test
port = 2222
user = carol
pass = BrandNewPass!
";
        let res_with_pass = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: replace_with_pass.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res_with_pass.applied.len(), 1);
        assert_eq!(
            env.store.get(&format!("server_{initial_id}")).unwrap(),
            "BrandNewPass!"
        );
        for key in &old_keys {
            if key == &format!("server_{initial_id}") {
                continue;
            }
            assert!(
                env.store.get(key).is_err(),
                "stale key {key} must be deleted even when new target supplies a password"
            );
        }
    }

    #[test]
    fn skip_mode_distinguishes_duplicate_in_template_from_existing_profile() {
        let env = TestVaultEnv::new();
        let existing_tpl = "\
[saved]
type = sftp
host = saved.example.test
user = alice
";
        apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: existing_tpl.to_string(),
            },
            &TemplateApplyOptions::default(),
        )
        .unwrap();

        let mut eval_saved = evaluate_template_text(
            "[saved]\ntype = sftp\nhost = saved2.example.test\nuser = alice\n",
            None,
            "n8-test",
            &crate::rclone_import::TemplateSecretPolicy::default(),
        )
        .unwrap();
        let eval_fresh1 = evaluate_template_text(
            "[fresh]\ntype = sftp\nhost = fresh1.example.test\nuser = bob\n",
            None,
            "n8-test",
            &crate::rclone_import::TemplateSecretPolicy::default(),
        )
        .unwrap();
        let eval_fresh2 = evaluate_template_text(
            "[fresh]\ntype = sftp\nhost = fresh2.example.test\nuser = bob\n",
            None,
            "n8-test",
            &crate::rclone_import::TemplateSecretPolicy::default(),
        )
        .unwrap();
        eval_saved.servers.extend(eval_fresh1.servers);
        eval_saved.servers.extend(eval_fresh2.servers);
        eval_saved.total_blocks = eval_saved.servers.len();

        let (current, _) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        let planned = build_plan(
            &eval_saved,
            &current,
            ExistingProfileMode::Skip,
            &RelaxationPolicy::ResetAll,
        );
        assert_eq!(planned.len(), 3);
        assert_eq!(planned[0].row.action, TemplatePlannedAction::Skip);
        assert!(!planned[0].row.duplicate_in_template);
        assert_eq!(
            planned[0].row.secret_note.as_deref(),
            Some("skipped existing")
        );
        assert_eq!(planned[1].row.action, TemplatePlannedAction::Add);
        assert!(!planned[1].row.duplicate_in_template);
        assert_eq!(planned[2].row.action, TemplatePlannedAction::Skip);
        assert!(planned[2].row.duplicate_in_template);
        assert_eq!(
            planned[2].row.secret_note.as_deref(),
            Some("duplicate in template")
        );

        let res = apply_eval_with_slots(
            &env.store,
            env.user_id,
            &crate::VaultSecretSlots {
                store: &env.store,
                user: env.user_id,
            },
            eval_saved,
            "n8-digest".to_string(),
            &RelaxationPolicy::ResetAll,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Skip,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(res.applied.len(), 1);
        assert_eq!(res.applied[0].target_name, "fresh");
        assert_eq!(res.skipped_existing, vec!["saved"]);
        assert_eq!(res.skipped_duplicate_in_template, vec!["fresh"]);
    }

    #[test]
    fn overwrite_target_reset_rollback_restores_partition_oauth_and_jotta_secrets() {
        let _up_guard = user_partitions::test_lock();
        let env = TestVaultEnv::new();
        // Add a second (non-legacy) user so `"oauth"` and `"jottacloud_refresh"`
        // are partition-private (`private_token_user` returns `Some(other_id)`).
        let other_user =
            user_partitions::cli_create_user(&env.store, "partner", None, None, None).unwrap();
        let other_id = other_user.id;
        user_partitions::cli_set_active_user(&env.store, other_id).unwrap();
        user_partitions::ensure_credential_user_unlocked(other_id).unwrap();

        let seed_json = vec![serde_json::json!({
            "id": "srv_oauth_1",
            "name": "acct",
            "protocol": "googledrive",
            "host": "www.googleapis.com",
            "port": 443,
            "username": "old",
            "hasStoredCredential": true
        })];
        user_partitions::cli_replace_server_profiles_for_user(&env.store, other_id, &seed_json)
            .unwrap();
        let oauth_key = "oauth_google_srv_oauth_1";
        user_partitions::store_imported_credential_for_user(
            &env.store,
            other_id,
            oauth_key,
            Some("oauth"),
            "{\"access_token\":\"orig-tok\"}",
        )
        .unwrap();

        struct FailAfterDeleteSlots<'a> {
            inner: crate::VaultSecretSlots<'a>,
            fail_key: String,
        }
        impl crate::profile_import_plan::SecretSlots for FailAfterDeleteSlots<'_> {
            type Snapshot =
                <crate::VaultSecretSlots<'static> as crate::profile_import_plan::SecretSlots>::Snapshot;
            fn snapshot(
                &self,
                write: &crate::profile_import_plan::SecretWrite,
            ) -> Result<Self::Snapshot, String> {
                self.inner.snapshot(write)
            }
            fn write(&self, write: &crate::profile_import_plan::SecretWrite) -> Result<(), String> {
                if write.key == self.fail_key {
                    return Err("injected write failure".to_string());
                }
                self.inner.write(write)
            }
            fn restore(
                &self,
                write: &crate::profile_import_plan::SecretWrite,
                before: Self::Snapshot,
            ) -> Result<(), String> {
                self.inner.restore(write, before)
            }
        }

        let tpl = "\
[acct]
type = sftp
host = new.example.test
user = alice
pass = FailOnThisWrite!
";
        let eval = evaluate_template_text(
            tpl,
            None,
            "rollback-test",
            &crate::rclone_import::TemplateSecretPolicy {
                default_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let err = apply_eval_with_slots(
            &env.store,
            other_id,
            &FailAfterDeleteSlots {
                inner: crate::VaultSecretSlots {
                    store: &env.store,
                    user: other_id,
                },
                // Fail on the last reset key AFTER `oauth_google_srv_oauth_1`
                // has already been deleted so rollback must restore it.
                fail_key: "oauth_fourshared_srv_oauth_1".to_string(),
            },
            eval,
            "rollback-digest".to_string(),
            &RelaxationPolicy::ResetAll,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("oauth_fourshared_srv_oauth_1"), "{err}");

        let restored =
            user_partitions::resolve_credential_for_user(&env.store, other_id, oauth_key)
                .expect("resolve credential after rollback");
        assert_eq!(
            restored.as_deref().map(|s| s.as_str()),
            Some("{\"access_token\":\"orig-tok\"}"),
            "partition OAuth token must be restored on reset rollback"
        );
    }

    #[test]
    fn plan_digest_detects_in_place_replacement_target_host_change_under_same_id_and_name() {
        let env = TestVaultEnv::new();
        let initial = vec![serde_json::json!({
            "id": "srv_1",
            "name": "nas",
            "protocol": "sftp",
            "host": "old-a.example.test",
            "port": 22,
            "username": "alice",
            "hasStoredCredential": false
        })];
        user_partitions::cli_replace_server_profiles_for_user(&env.store, env.user_id, &initial)
            .unwrap();

        let tpl = "[nas]\ntype = sftp\nhost = new.example.test\nuser = alice\n";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let preview = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
            &initial,
        )
        .unwrap();

        // Mutate the existing replacement target host under the SAME id and name.
        let mutated = vec![serde_json::json!({
            "id": "srv_1",
            "name": "nas",
            "protocol": "sftp",
            "host": "old-b.example.test",
            "port": 22,
            "username": "alice",
            "hasStoredCredential": false
        })];
        user_partitions::cli_replace_server_profiles_for_user(&env.store, env.user_id, &mutated)
            .unwrap();

        let err = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                expected_digest_sha256: Some(preview.digest_sha256),
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(
            err.to_lowercase().contains("changed") || err.to_lowercase().contains("refresh"),
            "unexpected error on mutated replacement target: {err}"
        );
    }

    #[test]
    fn apply_with_kept_relaxations_succeeds_without_false_stale_preview_error() {
        let env = TestVaultEnv::new();
        let tpl = "\
[dav]
type = webdav
url = https://dav.example.test
user = alice
pass = dav-pass
override.no_check_certificate = true
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let preview = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
        let keep_relaxed: Vec<KeptRelaxation> = preview
            .security
            .iter()
            .flat_map(|sec| {
                sec.reset.iter().map(|r| KeptRelaxation {
                    file_index: sec.file_index,
                    key: r.key.clone(),
                })
            })
            .collect();
        assert!(!keep_relaxed.is_empty());

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                default_secret_form: Some(TemplateSecretForm::Clear),
                keep_relaxed,
                expected_digest_sha256: Some(preview.digest_sha256),
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .expect("applying kept relaxations with unchanged saved profiles must succeed");
        assert_eq!(applied.applied.len(), 1);
        assert_eq!(
            applied.stored[0]
                .pointer("/options/verifyCert")
                .and_then(Value::as_bool),
            Some(false)
        );

        // Overwrite an existing safe HTTPS WebDAV profile (which has stored
        // password) with `override.no_check_certificate = true` and NO password
        // while keeping `verifyCert = false`. Under `ResetAll` in preview,
        // `same_target` is true (`KeptExisting`); under `Keep` in apply,
        // `verifyCert=false` flips `same_target` to false (`ClearedExisting` +
        // `resetTargetSecrets = true`), which previously triggered a false
        // stale-preview error.
        let existing_dav = vec![serde_json::json!({
            "id": "srv_dav_safe",
            "name": "dav2",
            "protocol": "webdav",
            "providerId": "custom-webdav",
            "host": "dav2.example.test",
            "port": 443,
            "username": "alice",
            "options": { "webdavScheme": "https" },
            "hasStoredCredential": true
        })];
        user_partitions::cli_replace_server_profiles_for_user(
            &env.store,
            env.user_id,
            &existing_dav,
        )
        .unwrap();
        let insecure_tpl = "\
[dav2]
type = webdav
url = https://dav2.example.test
user = alice
override.no_check_certificate = true
";
        let insecure_input = TemplateInput::Text {
            content: insecure_tpl.to_string(),
        };
        let insecure_preview = preview_template_with_current(
            &insecure_input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
            &existing_dav,
        )
        .unwrap();
        let insecure_keep: Vec<KeptRelaxation> = insecure_preview
            .security
            .iter()
            .flat_map(|sec| {
                sec.reset.iter().map(|r| KeptRelaxation {
                    file_index: sec.file_index,
                    key: r.key.clone(),
                })
            })
            .collect();
        assert!(!insecure_keep.is_empty());
        let insecure_applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &insecure_input,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                keep_relaxed: insecure_keep,
                expected_digest_sha256: Some(insecure_preview.digest_sha256),
                expected_plan_digest_sha256: Some(insecure_preview.plan_digest_sha256),
                ..Default::default()
            },
        )
        .expect("keeping verifyCert relaxation on overwrite must not fail stale preview check");
        assert_eq!(insecure_applied.applied.len(), 1);
        assert_eq!(
            insecure_applied.stored[0]
                .pointer("/options/verifyCert")
                .and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn alias_and_crypt_inherit_base_secret_form_requirement() {
        let env = TestVaultEnv::new();
        let crypt_pw = crate::rclone_import::obscure_password("vault-pass").unwrap();
        let tpl = format!(
            "[base]\ntype = sftp\nhost = nas.example.test\nuser = alice\npass = PlainSecret!\n\n\
             [work]\ntype = alias\nremote = base:docs\n\n\
             [vault]\ntype = crypt\nremote = base:enc\npassword = {crypt_pw}\n"
        );
        let input = TemplateInput::Text {
            content: tpl.clone(),
        };
        let mut vault_only_forms = std::collections::HashMap::new();
        vault_only_forms.insert("vault".to_string(), TemplateSecretForm::Obscured);
        let preview = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                secret_forms: vault_only_forms.clone(),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
        for name in ["base", "work", "vault"] {
            let row = preview
                .rows
                .iter()
                .find(|r| r.block_name == name)
                .unwrap_or_else(|| panic!("missing row {name}"));
            assert!(
                row.has_secret_input,
                "{name} must report has_secret_input=true: {row:?}"
            );
            assert!(
                row.needs_secret_form_choice,
                "{name} must require secret form when base secret form is unchosen: {row:?}"
            );
        }

        // Now resolve `[base]` via per-block policy without repeating `--secret-form-for work`:
        // `work` and `vault` must inherit `[base]`'s secret and `work` must reflect the form.
        let mut with_base_forms = vault_only_forms;
        with_base_forms.insert("base".to_string(), TemplateSecretForm::Clear);
        let preview_ok = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                secret_forms: with_base_forms.clone(),
                ..Default::default()
            },
            &[],
        )
        .unwrap();
        let work_row = preview_ok
            .rows
            .iter()
            .find(|r| r.block_name == "work")
            .unwrap();
        assert!(!work_row.needs_secret_form_choice);
        assert!(work_row.has_credential);
        assert_eq!(work_row.secret_form, Some(TemplateSecretForm::Clear));

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                secret_forms: with_base_forms,
                expected_digest_sha256: Some(preview_ok.digest_sha256),
                expected_plan_digest_sha256: Some(preview_ok.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(applied.applied.len(), 3);
        for s in &applied.stored {
            let id = s.get("id").and_then(Value::as_str).unwrap();
            let pw = user_partitions::resolve_credential_for_user(
                &env.store,
                env.user_id,
                &format!("server_{id}"),
            )
            .unwrap();
            assert_eq!(
                pw.as_deref().map(|z| z.as_str()),
                Some("PlainSecret!"),
                "stored secret missing for {:?}",
                s.get("name")
            );
        }
    }

    #[test]
    fn delete_rollback_restores_every_per_profile_reset_key_across_vault_and_partition() {
        struct FailingLastDeleteSlots<'a> {
            inner: crate::VaultSecretSlots<'a>,
            remaining_until_fail: std::cell::Cell<usize>,
        }

        impl SecretSlots for FailingLastDeleteSlots<'_> {
            type Snapshot = user_partitions::CredentialSnapshot;

            fn snapshot(&self, write: &SecretWrite) -> Result<Self::Snapshot, String> {
                self.inner.snapshot(write)
            }

            fn write(&self, write: &SecretWrite) -> Result<(), String> {
                if write.delete {
                    let rem = self.remaining_until_fail.get();
                    if rem == 0 {
                        return Err(format!("simulated last delete failure for {}", write.key));
                    }
                    self.remaining_until_fail.set(rem - 1);
                }
                self.inner.write(write)
            }

            fn restore(&self, write: &SecretWrite, before: Self::Snapshot) -> Result<(), String> {
                self.inner.restore(write, before)
            }
        }

        let env = TestVaultEnv::new();
        let seed_tpl = "\
[prod-sftp]
type = sftp
host = old.example.test
port = 22
user = alice
pass = InitialSecret!
";
        let seed = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: seed_tpl.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let id = seed.applied[0].profile_id.clone();

        // Populate EVERY key returned by `per_profile_vault_keys_for_target_reset`
        // with its real writer `credential_type` across both vault and partition.
        let reset_keys = crate::per_profile_vault_keys_for_target_reset(&id, &[]);
        assert!(
            reset_keys.len() >= 20,
            "expected complete per-profile key surface"
        );
        let mut seeded: Vec<(String, &'static str, String)> = Vec::new();
        for (idx, key) in reset_keys.iter().enumerate() {
            let kind: &'static str = if key.starts_with("oauth_")
                || crate::profile_auth::DURABLE_KEY_PREFIXES
                    .iter()
                    .any(|p| key.starts_with(p))
                || crate::profile_auth::TRANSIENT_KEY_PREFIXES
                    .iter()
                    .any(|p| key.starts_with(p))
            {
                "oauth"
            } else {
                user_partitions::relocate_secret_kind(key)
            };
            let val = format!("secret-{idx}-{key}");
            if key != &format!("server_{id}") {
                user_partitions::store_credential_for_user_typed_dual(
                    &env.store,
                    env.user_id,
                    key,
                    kind,
                    &val,
                )
                .unwrap();
                seeded.push((key.clone(), kind, val));
            } else {
                seeded.push((key.clone(), "server", "InitialSecret!".to_string()));
            }
        }

        let moved_tpl = "\
[prod-sftp]
type = sftp
host = new.example.test
port = 22
user = alice
";
        // Fail on the LAST delete so EVERY preceding key actually runs its
        // partition + vault delete and must be restored from CredentialSnapshot.
        let failing_slots = FailingLastDeleteSlots {
            inner: crate::VaultSecretSlots {
                store: &env.store,
                user: env.user_id,
            },
            remaining_until_fail: std::cell::Cell::new(reset_keys.len() - 1),
        };
        let err = apply_template_with_slots(
            &env.store,
            env.user_id,
            &failing_slots,
            &TemplateInput::Text {
                content: moved_tpl.to_string(),
            },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(err.contains("could not be cleared"), "{err}");

        let conn = user_partitions::open_or_init_cli().unwrap();
        let types: HashMap<String, String> =
            user_partitions::list_user_credential_ids_for(&conn, env.user_id)
                .unwrap()
                .into_iter()
                .collect();
        for (key, expected_kind, expected_val) in &seeded {
            assert_eq!(
                env.store.get(key).as_deref().ok(),
                Some(expected_val.as_str()),
                "vault secret not rolled back for {key}"
            );
            let resolved =
                user_partitions::resolve_credential_for_user(&env.store, env.user_id, key).unwrap();
            assert_eq!(
                resolved.as_deref().map(|z| z.as_str()),
                Some(expected_val.as_str()),
                "partition secret not rolled back for {key}"
            );
            assert_eq!(
                types.get(key).map(String::as_str),
                Some(*expected_kind),
                "partition credential_type not preserved across rollback for {key}"
            );
        }
    }

    #[test]
    fn existing_target_digest_ignores_volatile_metadata_but_refuses_acted_fields() {
        let env = TestVaultEnv::new();
        let write_profiles = |new_list: Vec<Value>| {
            let (_cur, rev) =
                user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id)
                    .unwrap();
            user_partitions::cli_update_profiles_at_revision(
                &env.store,
                env.user_id,
                &rev,
                |list| {
                    *list = new_list;
                },
            )
            .unwrap();
        };
        let initial_profile = vec![serde_json::json!({
            "id": "srv_digest_1",
            "name": "prod-dav",
            "protocol": "webdav",
            "providerId": "custom-webdav",
            "host": "dav.example.test",
            "port": 443,
            "username": "alice",
            "initialPath": "/docs",
            "hasStoredCredential": true,
            "lastConnected": "2026-11-10T10:00:00Z",
            "quota": { "used": 100, "total": 1000 },
            "quotaCache": { "bytes": 100 },
            "favicon": "data:image/png;base64,AAA",
            "connectFailures": 0,
            "options": {
                "basePath": "/dav/alice",
                "verifyCert": true
            }
        })];
        write_profiles(initial_profile.clone());

        let tpl = "\
[prod-dav]
type = webdav
url = https://dav.example.test/dav/alice
user = alice
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let (current, _) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        let preview = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Overwrite,
                ..Default::default()
            },
            &current,
        )
        .unwrap();

        // 1. Mutating volatile non-acted fields (lastConnected, quotaCache,
        //    favicon, connectFailures) MUST NOT invalidate the preview digest.
        let mut volatile_update = current.clone();
        let obj = volatile_update[0].as_object_mut().unwrap();
        obj.insert(
            "lastConnected".to_string(),
            Value::String("2026-11-15T12:34:56Z".to_string()),
        );
        obj.insert(
            "quota".to_string(),
            serde_json::json!({ "used": 555, "total": 1000 }),
        );
        obj.insert(
            "quotaCache".to_string(),
            serde_json::json!({ "bytes": 555 }),
        );
        obj.insert(
            "favicon".to_string(),
            Value::String("data:image/png;base64,BBB".to_string()),
        );
        obj.insert("connectFailures".to_string(), Value::from(3));
        write_profiles(volatile_update);

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Overwrite,
                expected_digest_sha256: Some(preview.digest_sha256.clone()),
                expected_plan_digest_sha256: Some(preview.plan_digest_sha256.clone()),
                ..Default::default()
            },
        )
        .expect("mutating only lastConnected/quotaCache/favicon/connectFailures must not refuse");
        assert_eq!(applied.applied.len(), 1);

        // 2. Mutating EVERY acted-on field on the matched target MUST refuse.
        type ProfileMutation = (&'static str, fn(&mut serde_json::Map<String, Value>));
        let mutations: Vec<ProfileMutation> = vec![
            ("host", |m| {
                m.insert("host".into(), Value::String("other.example.test".into()));
            }),
            ("port", |m| {
                m.insert("port".into(), Value::from(8443));
            }),
            ("username", |m| {
                m.insert("username".into(), Value::String("bob".into()));
            }),
            ("protocol", |m| {
                m.insert("protocol".into(), Value::String("sftp".into()));
            }),
            ("providerId", |m| {
                m.insert("providerId".into(), Value::String("nextcloud".into()));
            }),
            ("initialPath", |m| {
                m.insert("initialPath".into(), Value::String("/other".into()));
            }),
            ("options.verifyCert", |m| {
                m.insert(
                    "options".into(),
                    serde_json::json!({ "basePath": "/dav/alice", "verifyCert": false }),
                );
            }),
            ("id", |m| {
                m.insert("id".into(), Value::String("srv_digest_other".into()));
            }),
        ];

        for (label, mutate) in mutations {
            write_profiles(initial_profile.clone());
            let (base_current, _) =
                user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id)
                    .unwrap();
            let fresh_preview = preview_template_with_current(
                &input,
                &TemplatePreviewOptions {
                    existing: ExistingProfileMode::Overwrite,
                    ..Default::default()
                },
                &base_current,
            )
            .unwrap();

            let mut edited = base_current.clone();
            mutate(edited[0].as_object_mut().unwrap());
            write_profiles(edited);

            let err = apply_template_for_user(
                &env.store,
                env.user_id,
                &input,
                &TemplateApplyOptions {
                    existing: ExistingProfileMode::Overwrite,
                    expected_digest_sha256: Some(fresh_preview.digest_sha256),
                    expected_plan_digest_sha256: Some(fresh_preview.plan_digest_sha256),
                    ..Default::default()
                },
            )
            .expect_err(&format!(
                "mutating acted-on `{label}` must refuse stale plan"
            ));
            assert!(
                err.contains("Saved profiles changed"),
                "unexpected error for `{label}`: {err}"
            );
        }
    }

    #[test]
    fn base_exists_skip_alias_new_asks_once_and_decodes_from_alias_answer() {
        let env = TestVaultEnv::new();
        let seed_tpl = "\
[base]
type = sftp
host = nas.example.test
user = alice
pass = ExistingBasePw!
";
        let seeded = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text {
                content: seed_tpl.to_string(),
            },
            &TemplateApplyOptions {
                default_secret_form: Some(TemplateSecretForm::Clear),
                ..Default::default()
            },
        )
        .unwrap();
        let base_id = seeded.applied[0].profile_id.clone();

        // Scenario A: `base` exists with `Skip` mode, `work` is an `alias` of `base`.
        let tpl = "\
[base]
type = sftp
host = nas.example.test
user = alice
pass = AliasClearSecret!

[work]
type = alias
remote = base:docs
";
        let input = TemplateInput::Text {
            content: tpl.to_string(),
        };
        let (current, _) =
            user_partitions::cli_read_active_profiles_at_revision(&env.store, env.user_id).unwrap();
        let preview1 = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Skip,
                ..Default::default()
            },
            &current,
        )
        .unwrap();
        assert_eq!(preview1.rows[0].block_name, "base");
        assert_eq!(preview1.rows[0].action, TemplatePlannedAction::Skip);
        assert_eq!(preview1.rows[1].block_name, "work");
        assert_eq!(preview1.rows[1].action, TemplatePlannedAction::Add);
        assert!(preview1.rows[1].needs_secret_form_choice);
        assert_eq!(preview1.secret_choice_blocks, vec!["work"]);

        // User answers ONLY for `work` (Clear); `base` was skipped so not answered.
        let mut work_only = std::collections::HashMap::new();
        work_only.insert("work".to_string(), TemplateSecretForm::Clear);
        let preview2 = preview_template_with_current(
            &input,
            &TemplatePreviewOptions {
                existing: ExistingProfileMode::Skip,
                secret_forms: work_only.clone(),
                ..Default::default()
            },
            &current,
        )
        .unwrap();
        assert!(preview2.secret_choice_blocks.is_empty());
        assert!(!preview2.rows[1].needs_secret_form_choice);
        assert_eq!(
            preview2.rows[1].secret_form,
            Some(TemplateSecretForm::Clear)
        );
        assert!(preview2.rows[1].has_credential);

        let applied = apply_template_for_user(
            &env.store,
            env.user_id,
            &input,
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Skip,
                secret_forms: work_only.clone(),
                expected_digest_sha256: Some(preview2.digest_sha256),
                expected_plan_digest_sha256: Some(preview2.plan_digest_sha256),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(applied.skipped_existing, vec!["base"]);
        assert_eq!(applied.applied.len(), 1);
        assert_eq!(applied.applied[0].target_name, "work");
        let work_id = applied.applied[0].profile_id.clone();

        let base_pw = user_partitions::resolve_credential_for_user(
            &env.store,
            env.user_id,
            &format!("server_{base_id}"),
        )
        .unwrap();
        assert_eq!(
            base_pw.as_deref().map(|z| z.as_str()),
            Some("ExistingBasePw!")
        );
        let work_pw = user_partitions::resolve_credential_for_user(
            &env.store,
            env.user_id,
            &format!("server_{work_id}"),
        )
        .unwrap();
        assert_eq!(
            work_pw.as_deref().map(|z| z.as_str()),
            Some("AliasClearSecret!")
        );

        // Scenario B: `base` deselected via `selected_blocks = ["work"]` while `base` has obscured password.
        let obscured = crate::rclone_import::obscure_password("ObscuredAliasPw!").unwrap();
        let obs_tpl = format!(
            "[base2]\ntype = sftp\nhost = nas.example.test\nuser = alice\npass = {obscured}\n\n\
             [work2]\ntype = alias\nremote = base2:sub\n"
        );
        let mut work2_only = std::collections::HashMap::new();
        work2_only.insert("work2".to_string(), TemplateSecretForm::Obscured);
        let applied_desel = apply_template_for_user(
            &env.store,
            env.user_id,
            &TemplateInput::Text { content: obs_tpl },
            &TemplateApplyOptions {
                existing: ExistingProfileMode::Skip,
                selected_blocks: Some(vec!["work2".to_string()]),
                secret_forms: work2_only,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(applied_desel.skipped_unselected, vec!["base2"]);
        assert_eq!(applied_desel.applied.len(), 1);
        let work2_id = applied_desel.applied[0].profile_id.clone();
        let work2_pw = user_partitions::resolve_credential_for_user(
            &env.store,
            env.user_id,
            &format!("server_{work2_id}"),
        )
        .unwrap();
        assert_eq!(
            work2_pw.as_deref().map(|z| z.as_str()),
            Some("ObscuredAliasPw!")
        );
    }
}
