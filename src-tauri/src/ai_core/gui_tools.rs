use crate::ai_core::local_tools::{get_str_opt, resolve_local_path, validate_path};
use crate::ai_core::tools::ToolError;
use crate::ai_tools::{
    create_temp_provider, download_from_provider, emit_tool_progress, find_server_by_name_or_id,
    has_ftp, has_provider, join_remote_path, load_saved_servers, path_basename,
    validate_remote_path,
};
use serde_json::{json, Value};
use tauri::{Emitter, Manager};

fn get_str_s(args: &Value, key: &str) -> Result<String, String> {
    crate::ai_core::local_tools::get_str(args, key).map_err(|e| e.to_string())
}
fn get_str_array_s(args: &Value, key: &str) -> Result<Vec<String>, String> {
    crate::ai_core::local_tools::value_as_string_array(args, key).map_err(|e| e.to_string())
}

/// Interim crypt-overlay guard for the AeroAgent `gui_tools` paths.
///
/// Delegates to the shared [`ProviderState::guard_no_raw_crypt_write`], which
/// refuses a direct write to the RAW `ProviderState::provider` while the session
/// is crypt-CAPABLE but currently UNWRAPPED (badge locked / outside the encrypted
/// scope). The same guard now protects the regular GUI command layer
/// (`provider_upload_file`/`_folder`, mkdir/delete/rename/server-copy); the
/// `gui_tools` read paths (`download_files`, `sync_preview`, `remote_edit`) call
/// it too so a raw read never returns ciphertext. The CLI, cross-profile/agent
/// transfer, and MCP backends are crypt-aware (Phases 1-2), so steer the agent
/// there.
fn guard_no_raw_crypt_write(
    state: &tauri::State<'_, crate::provider_commands::ProviderState>,
    op: &str,
) -> Result<(), String> {
    state.guard_no_raw_crypt_write(op)
}

/// Publish the edited text staged at `local_temp` over `path` on the
/// connected provider (AeroAgent `remote_edit`), the way the CLI `edit` and
/// the MCP `aeroftp_edit` do: a temporary beside the file, then `replace`.
///
/// Asked first (G119): without an atomic replace the edit is refused before
/// anything is written, unless `allow_non_atomic` is set and the backend's
/// replace sets the previous file aside. An upload over the file in place,
/// what this path did until 4.2.0, leaves a partial file when it breaks.
///
/// The replace puts a new file in the target's place, which the upload in
/// place did not: a symbolic link is refused before anything is staged,
/// and the mode of the file is set on the temporary before the replace. A
/// mode that could not be set is reported through
/// [`crate::providers::report_warning`] once the edit is done.
async fn publish_remote_edit(
    provider: &mut dyn crate::providers::StorageProvider,
    local_temp: &str,
    path: &str,
    allow_non_atomic: bool,
) -> Result<(), String> {
    crate::providers::ensure_edit_can_replace(
        provider,
        path,
        allow_non_atomic,
        "`allow_non_atomic` true",
    )
    .await
    .map_err(|e| e.to_string())?;
    let original = crate::providers::inspect_edit_target(provider, path)
        .await
        .map_err(|e| e.to_string())?;
    let remote_temp = crate::ai_core::remote_tools::edit_temp_path(path);
    if let Err(e) = provider.upload(local_temp, &remote_temp, None).await {
        let _ = provider.delete(&remote_temp).await;
        return Err(e.to_string());
    }
    let not_kept =
        crate::providers::keep_edit_original(provider, &remote_temp, path, &original).await;
    // `replace` and not `rename`: the destination exists by definition.
    if let Err(e) = provider.replace(&remote_temp, path).await {
        let _ = provider.delete(&remote_temp).await;
        return Err(e.to_string());
    }
    if let Some(warning) = not_kept {
        tracing::warn!("{warning}");
        crate::providers::report_warning(warning);
    }
    Ok(())
}

/// [`publish_remote_edit`] on the legacy FTP session ([`crate::ftp::FtpManager`],
/// the GUI's `connect_ftp` with no provider active), which until 4.2.1 stored
/// the new text over the file in place. The manager is lent to a
/// [`FtpManagerProvider`](super::ftp_manager_provider::FtpManagerProvider) for
/// the publish and put back afterwards, also if the future is dropped.
async fn publish_remote_edit_on_ftp_manager(
    manager: &mut crate::ftp::FtpManager,
    local_temp: &str,
    path: &str,
    allow_non_atomic: bool,
) -> Result<(), String> {
    struct Lent<'a> {
        slot: &'a mut crate::ftp::FtpManager,
        session: super::ftp_manager_provider::FtpManagerProvider,
    }
    impl Drop for Lent<'_> {
        fn drop(&mut self) {
            std::mem::swap(self.slot, &mut self.session.0);
        }
    }
    let session = std::mem::take(manager);
    let mut lent = Lent {
        slot: manager,
        session: super::ftp_manager_provider::FtpManagerProvider(session),
    };
    publish_remote_edit(&mut lent.session, local_temp, path, allow_non_atomic).await
}

pub async fn dispatch_gui_tool(
    ctx: &dyn crate::ai_core::tools::ToolCtx,
    tool_name: &str,
    args: &Value,
) -> Result<Value, ToolError> {
    let app = ctx
        .tauri_app_handle()
        .ok_or_else(|| ToolError::Exec("Requires GUI".into()))?;
    let state = app.state::<crate::provider_commands::ProviderState>();
    let app_state = app.state::<crate::AppState>();
    let context_local_path = ctx.context_local_path().map(|s| s.to_string());

    let result: Result<Value, String> = async {
        match tool_name {
        "set_theme" => {

            let theme = get_str_s(args, "theme")?;
            let valid_themes = ["light", "dark", "tokyo", "cyber"];
            if !valid_themes.contains(&theme.as_str()) {
                return Err(format!(
                    "Invalid theme '{}'. Valid themes: {}",
                    theme,
                    valid_themes.join(", ")
                ));
            }
            app.emit("ai-set-theme", json!({ "theme": theme }))
                .map_err(|e| format!("Failed to emit theme event: {}", e))?;
            Ok(json!({
                "success": true,
                "theme": theme,
                "message": format!("Theme changed to '{}'", theme)
            }))

        }
        "app_info" => {

            let version = env!("CARGO_PKG_VERSION");
            let os = std::env::consts::OS;
            let arch = std::env::consts::ARCH;

            let has_prov = has_provider(&state).await;
            let has_ftp_conn = has_ftp(&app_state).await;

            let mut info = json!({
                "version": version,
                "platform": os,
                "arch": arch,
                "connected": has_prov || has_ftp_conn,
                "connection_type": if has_prov { "provider" } else if has_ftp_conn { "ftp" } else { "none" },
            });

            if let Some(ref local_path) = context_local_path {
                info["current_local_path"] = json!(local_path);
            }

            Ok(info)

        }
        "sync_control" => {

            let action = get_str_s(args, "action")?;
            match action.as_str() {
                "status" => {
                    let running =
                        crate::BACKGROUND_SYNC_RUNNING.load(std::sync::atomic::Ordering::SeqCst);
                    Ok(json!({
                        "success": true,
                        "sync_running": running,
                        "message": if running { "Background sync is running" } else { "Background sync is not running" }
                    }))
                }
                "start" => {
                    app.emit("ai-sync-control", json!({ "action": "start" }))
                        .map_err(|e| format!("Failed to emit sync control event: {}", e))?;
                    Ok(json!({
                        "success": true,
                        "message": "Background sync start requested"
                    }))
                }
                "stop" => {
                    app.emit("ai-sync-control", json!({ "action": "stop" }))
                        .map_err(|e| format!("Failed to emit sync control event: {}", e))?;
                    Ok(json!({
                        "success": true,
                        "message": "Background sync stop requested"
                    }))
                }
                _ => Err(format!(
                    "Invalid sync action '{}'. Use: start, stop, status",
                    action
                )),
            }

        }
        "vault_peek" => {

            let path = resolve_local_path(&get_str_s(args, "path")?, context_local_path.as_deref());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.exists() {
                return Err(format!("Vault file not found: {}", path));
            }
            if !path.ends_with(".aerovault") {
                return Err("File is not an AeroVault container (.aerovault)".to_string());
            }

            let info = crate::aerovault_v2::vault_v2_peek(path).await?;
            Ok(info)

        }
        "cross_profile_transfer" => {

            use crate::cross_profile_transfer::{
                copy_one_file, plan_transfer, should_skip_existing, CrossProfileTransferRequest,
            };

            let source_server_query = get_str_s(args, "source_server")?;
            let dest_server_query = get_str_s(args, "dest_server")?;
            let source_path = get_str_s(args, "source_path")?;
            let dest_path = get_str_s(args, "dest_path")?;
            let recursive = args
                .get("recursive")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let skip_existing = args
                .get("skip_existing")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let dry_run = args
                .get("dry_run")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            validate_remote_path(&source_path, "source_path")?;
            validate_remote_path(&dest_path, "dest_path")?;

            let servers = load_saved_servers()?;
            let source_server = find_server_by_name_or_id(&servers, &source_server_query)?;
            let dest_server = find_server_by_name_or_id(&servers, &dest_server_query)?;

            if source_server.id == dest_server.id {
                return Err("Source and destination must be different servers".to_string());
            }

            let mut source_provider = create_temp_provider(&source_server).await?;
            let mut dest_provider = create_temp_provider(&dest_server).await?;

            let request = CrossProfileTransferRequest {
                source_profile: source_server.name.clone(),
                dest_profile: dest_server.name.clone(),
                source_path,
                dest_path,
                recursive,
                dry_run,
                skip_existing,
            };

            let plan_result =
                plan_transfer(source_provider.as_mut(), dest_provider.as_mut(), &request).await;
            if let Err(err) = source_provider.disconnect().await {
                eprintln!(
                    "ai_tools: failed to disconnect source provider after planning: {}",
                    err
                );
            }
            if let Err(err) = dest_provider.disconnect().await {
                eprintln!(
                    "ai_tools: failed to disconnect destination provider after planning: {}",
                    err
                );
            }
            let plan = plan_result.map_err(|e| format!("Planning failed: {}", e))?;

            if dry_run || plan.entries.is_empty() {
                return Ok(json!({
                    "dry_run": true,
                    "source": source_server.name,
                    "destination": dest_server.name,
                    "total_files": plan.total_files,
                    "total_bytes": plan.total_bytes,
                    "entries": plan.entries.iter().map(|e| json!({
                        "source": e.source_path,
                        "dest": e.dest_path,
                        "size": e.size,
                    })).collect::<Vec<_>>(),
                }));
            }

            // Execute
            let mut transferred = 0u64;
            let mut skipped = 0u64;
            let mut failed = 0u64;
            let mut errors: Vec<String> = Vec::new();

            let mut source_provider = create_temp_provider(&source_server).await?;
            let mut dest_provider = create_temp_provider(&dest_server).await?;

            for entry in &plan.entries {
                if skip_existing {
                    if let Ok(true) =
                        should_skip_existing(dest_provider.as_mut(), &entry.dest_path, entry).await
                    {
                        skipped += 1;
                        continue;
                    }
                }
                match copy_one_file(
                    source_provider.as_mut(),
                    dest_provider.as_mut(),
                    &entry.source_path,
                    &entry.dest_path,
                    entry.modified.as_deref(),
                )
                .await
                {
                    Ok(()) => transferred += 1,
                    Err(e) => {
                        failed += 1;
                        if errors.len() < 5 {
                            errors.push(format!("{}: {}", entry.display_name, e));
                        }
                    }
                }
            }

            if let Err(err) = source_provider.disconnect().await {
                eprintln!(
                    "ai_tools: failed to disconnect source provider after execute: {}",
                    err
                );
            }
            if let Err(err) = dest_provider.disconnect().await {
                eprintln!(
                    "ai_tools: failed to disconnect destination provider after execute: {}",
                    err
                );
            }

            Ok(json!({
                "source": source_server.name,
                "destination": dest_server.name,
                "transferred": transferred,
                "skipped": skipped,
                "failed": failed,
                "errors": errors,
            }))

        }
        "preview_edit" => {

            let path = get_str_s(args, "path")?;
            let find = get_str_s(args, "find")?;
            let replace = get_str_s(args, "replace")?;
            let replace_all = args
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            let remote = args
                .get("remote")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            validate_path(&path, "path")?;

            const MAX_PREVIEW_BYTES: usize = 100 * 1024; // 100KB

            let mut content = if remote {
                guard_no_raw_crypt_write(&state, "preview_edit (remote)")?;
                let bytes = download_from_provider(&state, &app_state, &path).await?;
                if bytes.len() > MAX_PREVIEW_BYTES {
                    return Ok(json!({
                        "success": false,
                        "message": "File too large for preview (max 100KB)",
                    }));
                }
                String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8 text".to_string())?
            } else {
                let meta =
                    std::fs::metadata(&path).map_err(|e| format!("Failed to stat file: {}", e))?;
                if meta.len() as usize > MAX_PREVIEW_BYTES {
                    return Ok(json!({
                        "success": false,
                        "message": "File too large for preview (max 100KB)",
                    }));
                }
                std::fs::read_to_string(&path).map_err(|e| format!("Failed to read file: {}", e))?
            };

            // Strip UTF-8 BOM if present
            if content.starts_with('\u{FEFF}') {
                content = content.strip_prefix('\u{FEFF}').unwrap().to_string();
            }

            let occurrences = content.matches(&find).count();
            if occurrences == 0 {
                return Ok(json!({
                    "success": false,
                    "message": "String not found in file",
                    "occurrences": 0,
                }));
            }

            let modified_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };

            let replaced = if replace_all { occurrences } else { 1 };
            Ok(json!({
                "success": true,
                "original": content,
                "modified": modified_content,
                "occurrences": occurrences,
                "replaced": replaced,
            }))

        }
        "hash_file" => {

            let path = get_str_s(args, "path")?;
            let algorithm = get_str_opt(args, "algorithm").unwrap_or_else(|| "sha256".to_string());
            validate_path(&path, "path")?;

            let p = std::path::Path::new(&path);
            if !p.is_file() {
                return Err(format!("Path is not a file: {}", path));
            }

            // Delegate to existing cyber_tools::hash_file (default classic digest length)
            let hash =
                crate::cyber_tools::hash_file(path.clone(), algorithm.clone(), None, None, None).await?;

            Ok(json!({
                "path": path,
                "algorithm": algorithm,
                "hash": hash,
            }))

        }
        "generate_transfer_plan" => {

            let direction = get_str_s(args, "direction")?;
            let destination = get_str_s(args, "destination")?;
            let sources = get_str_array_s(args, "paths")?;
            if sources.is_empty() {
                return Err("'paths' must contain at least one source".to_string());
            }

            let mut warnings: Vec<String> = Vec::new();
            let mut operations: Vec<Value> = Vec::new();

            match direction.as_str() {
                "upload" => {
                    validate_path(&destination, "destination")?;
                    let mkdir_id = format!("plan_{}_mkdir_remote", uuid::Uuid::new_v4());
                    operations.push(json!({
                        "id": mkdir_id,
                        "toolName": "remote_mkdir",
                        "title": format!("Ensure remote directory {} exists", destination),
                        "description": "Create the destination directory on the remote server if needed.",
                        "category": "prepare",
                        "dangerLevel": "medium",
                        "args": { "path": destination.clone() },
                        "dependsOn": [],
                    }));

                    for raw_source in &sources {
                        let resolved_source =
                            resolve_local_path(raw_source, context_local_path.as_deref());
                        validate_path(&resolved_source, "path")?;
                        let source_path = std::path::Path::new(&resolved_source);
                        if !source_path.exists() {
                            warnings
                                .push(format!("Skipped missing local path: {}", resolved_source));
                            continue;
                        }

                        if source_path.is_dir() {
                            operations.push(json!({
                                "id": format!("plan_{}_upload_dir", uuid::Uuid::new_v4()),
                                "toolName": "upload_files",
                                "title": format!("Upload directory {}", resolved_source),
                                "description": format!("Recursively upload {} into {}.", resolved_source, destination),
                                "category": "upload",
                                "dangerLevel": "medium",
                                "args": {
                                    "paths": [resolved_source],
                                    "remote_dir": destination.clone(),
                                },
                                "dependsOn": [mkdir_id.clone()],
                            }));
                        } else if let Some(file_name) = path_basename(&resolved_source) {
                            operations.push(json!({
                                "id": format!("plan_{}_upload_file", uuid::Uuid::new_v4()),
                                "toolName": "remote_upload",
                                "title": format!("Upload {}", file_name),
                                "description": format!("Upload {} to {}.", resolved_source, join_remote_path(&destination, &file_name)),
                                "category": "upload",
                                "dangerLevel": "medium",
                                "args": {
                                    "local_path": resolved_source,
                                    "remote_path": join_remote_path(&destination, &file_name),
                                },
                                "dependsOn": [mkdir_id.clone()],
                            }));
                        }
                    }
                }
                "download" => {
                    let resolved_destination =
                        resolve_local_path(&destination, context_local_path.as_deref());
                    validate_path(&resolved_destination, "destination")?;
                    let mkdir_id = format!("plan_{}_mkdir_local", uuid::Uuid::new_v4());
                    operations.push(json!({
                        "id": mkdir_id,
                        "toolName": "local_mkdir",
                        "title": format!("Ensure local directory {} exists", resolved_destination),
                        "description": "Create the destination directory locally if needed.",
                        "category": "prepare",
                        "dangerLevel": "medium",
                        "args": { "path": resolved_destination.clone() },
                        "dependsOn": [],
                    }));

                    for remote_source in &sources {
                        validate_path(remote_source, "path")?;
                        match path_basename(remote_source) {
                            Some(file_name) => {
                                operations.push(json!({
                                    "id": format!("plan_{}_download_file", uuid::Uuid::new_v4()),
                                    "toolName": "remote_download",
                                    "title": format!("Download {}", file_name),
                                    "description": format!("Download {} into {}.", remote_source, resolved_destination),
                                    "category": "download",
                                    "dangerLevel": "medium",
                                    "args": {
                                        "remote_path": remote_source,
                                        "local_path": format!("{}/{}", resolved_destination.trim_end_matches('/'), file_name),
                                    },
                                    "dependsOn": [mkdir_id.clone()],
                                }));
                            }
                            None => warnings.push(format!(
                                "Skipped remote source without file name: {}",
                                remote_source
                            )),
                        }
                    }
                }
                _ => {
                    return Err(format!(
                        "Invalid transfer plan direction '{}'. Use 'upload' or 'download'.",
                        direction
                    ))
                }
            }

            let executable_operations = operations
                .iter()
                .filter(|op| {
                    op.get("category").and_then(Value::as_str) != Some("prepare")
                        || operations.len() == 1
                })
                .count();

            Ok(json!({
                "plan_kind": "transfer",
                "direction": direction,
                "destination": destination,
                "source_count": sources.len(),
                "operation_count": operations.len(),
                "executable_operations": executable_operations,
                "warnings": warnings,
                "summary": format!("Prepared {} {} operation(s) for {} source item(s).", operations.len(), direction, sources.len()),
                "operations": operations,
            }))

        }
        "upload_files" => {
            guard_no_raw_crypt_write(&state, "upload_files")?;

            let local_paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .ok_or("Missing 'paths' array parameter")?;
            let remote_dir = get_str_s(args, "remote_dir")?;
            validate_path(&remote_dir, "remote_dir")?;

            let mut uploaded = Vec::new();
            let mut errors = Vec::new();

            // Expand directories into individual file paths (recursive)
            let mut expanded_paths: Vec<(String, String)> = Vec::new(); // (local_path, relative_remote_path)
            for raw_path in &local_paths {
                let local_path = resolve_local_path(raw_path, context_local_path.as_deref());
                let p = std::path::Path::new(&local_path);
                if p.is_dir() {
                    // Walk directory recursively
                    let base = p.to_path_buf();
                    let mut stack = vec![base.clone()];
                    while let Some(dir) = stack.pop() {
                        let entries = std::fs::read_dir(&dir).map_err(|e| {
                            format!("Failed to read directory {}: {}", dir.display(), e)
                        })?;
                        for entry in entries {
                            let entry =
                                entry.map_err(|e| format!("Directory entry error: {}", e))?;
                            let entry_path = entry.path();
                            if entry_path.is_dir() {
                                stack.push(entry_path);
                            } else {
                                let rel = entry_path
                                    .strip_prefix(&base)
                                    .map(|r| r.to_string_lossy().to_string())
                                    .unwrap_or_else(|_| {
                                        entry_path
                                            .file_name()
                                            .unwrap_or_default()
                                            .to_string_lossy()
                                            .to_string()
                                    });
                                let dir_name = base
                                    .file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .to_string();
                                let remote_rel = format!("{}/{}", dir_name, rel);
                                expanded_paths
                                    .push((entry_path.to_string_lossy().to_string(), remote_rel));
                            }
                        }
                    }
                } else {
                    let filename = p
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "file".to_string());
                    expanded_paths.push((local_path, filename));
                }
            }

            let total = expanded_paths.len();
            for (idx, (local_path, rel_path)) in expanded_paths.iter().enumerate() {
                validate_path(local_path, "path").map_err(|e| e.to_string())?;
                let remote_path = format!("{}/{}", remote_dir.trim_end_matches('/'), rel_path);

                // Ensure remote parent directory exists (create each level)
                if let Some(parent) = std::path::Path::new(&remote_path).parent() {
                    let parent_str = parent.to_string_lossy().to_string();
                    let base = remote_dir.trim_end_matches('/');
                    if parent_str != base {
                        // Build list of directories to create, from shallowest to deepest
                        let rel_to_base = parent_str.strip_prefix(base).unwrap_or(&parent_str);
                        let parts: Vec<&str> =
                            rel_to_base.split('/').filter(|s| !s.is_empty()).collect();
                        let mut current = base.to_string();
                        for part in &parts {
                            current = format!("{}/{}", current, part);
                            if has_provider(&state).await {
                                let mut provider = state.provider.lock().await;
                                if let Some(p) = provider.as_mut() {
                                    let _ = p.mkdir(&current).await;
                                }
                            } else if has_ftp(&app_state).await {
                                let mut manager = app_state.ftp_manager.lock().await;
                                let _ = manager.mkdir(&current).await;
                            }
                        }
                    }
                }

                let display_name = rel_path.clone();
                emit_tool_progress(
                    &app,
                    "upload_files",
                    idx as u32 + 1,
                    total as u32,
                    &display_name,
                );

                let result = if has_provider(&state).await {
                    let mut provider = state.provider.lock().await;
                    let provider = match provider.as_mut() {
                        Some(p) => p,
                        None => return Err("No active provider connection".into()),
                    };
                    provider
                        .upload(local_path, &remote_path, None)
                        .await
                        .map_err(|e| e.to_string())
                } else if has_ftp(&app_state).await {
                    let mut manager = app_state.ftp_manager.lock().await;
                    manager
                        .upload_file(local_path, &remote_path)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    Err("Not connected to any server".to_string())
                };

                match result {
                    Ok(_) => uploaded.push(display_name),
                    Err(e) => errors.push(json!({ "file": display_name, "error": e })),
                }
            }

            Ok(json!({
                "uploaded": uploaded.len(),
                "failed": errors.len(),
                "files": uploaded,
                "errors": errors,
            }))

        }
        "download_files" => {
            guard_no_raw_crypt_write(&state, "download_files")?;

            let remote_paths: Vec<String> = args
                .get("paths")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .ok_or("Missing 'paths' array parameter")?;
            let local_dir =
                resolve_local_path(&get_str_s(args, "local_dir")?, context_local_path.as_deref());
            validate_path(&local_dir, "local_dir")?;

            // Ensure local dir exists
            std::fs::create_dir_all(&local_dir)
                .map_err(|e| format!("Failed to create local directory: {}", e))?;

            let mut downloaded = Vec::new();
            let mut errors = Vec::new();
            let total = remote_paths.len();

            for (idx, remote_path) in remote_paths.iter().enumerate() {
                validate_path(remote_path, "path").map_err(|e| e.to_string())?;
                let filename = std::path::Path::new(remote_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "file".to_string());
                let local_path = format!("{}/{}", local_dir.trim_end_matches('/'), filename);
                validate_path(&local_path, "local_path")?;

                emit_tool_progress(
                    &app,
                    "download_files",
                    idx as u32 + 1,
                    total as u32,
                    &filename,
                );

                let result = if has_provider(&state).await {
                    let mut provider = state.provider.lock().await;
                    let provider = match provider.as_mut() {
                        Some(p) => p,
                        None => return Err("No active provider connection".into()),
                    };
                    provider
                        .download(remote_path, &local_path, None)
                        .await
                        .map_err(|e| e.to_string())
                } else if has_ftp(&app_state).await {
                    let mut manager = app_state.ftp_manager.lock().await;
                    manager
                        .download_file(remote_path, &local_path)
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    Err("Not connected to any server".to_string())
                };

                match result {
                    Ok(_) => downloaded.push(filename),
                    Err(e) => errors.push(json!({ "file": filename, "error": e })),
                }
            }

            Ok(json!({
                "downloaded": downloaded.len(),
                "failed": errors.len(),
                "files": downloaded,
                "errors": errors,
            }))

        }
        "sync_preview" => {
            guard_no_raw_crypt_write(&state, "sync_preview")?;

            let local_path = get_str_s(args, "local_path")?;
            let remote_path = get_str_s(args, "remote_path")?;
            validate_path(&local_path, "local_path")?;
            validate_path(&remote_path, "remote_path")?;

            // Collect local files
            let local_files: std::collections::HashMap<String, u64> =
                std::fs::read_dir(&local_path)
                    .map_err(|e| format!("Failed to read local directory: {}", e))?
                    .filter_map(|e| e.ok())
                    .filter_map(|e| {
                        let meta = e.metadata().ok()?;
                        if meta.is_file() {
                            Some((e.file_name().to_string_lossy().to_string(), meta.len()))
                        } else {
                            None
                        }
                    })
                    .collect();

            // Collect remote files
            let remote_files: std::collections::HashMap<String, u64> = if has_provider(&state).await
            {
                let mut provider = state.provider.lock().await;
                let provider = match provider.as_mut() {
                    Some(p) => p,
                    None => return Err("No active provider connection".into()),
                };
                let entries = provider
                    .list(&remote_path)
                    .await
                    .map_err(|e| e.to_string())?;
                entries
                    .iter()
                    .filter(|e| !e.is_dir)
                    .map(|e| (e.name.clone(), e.size))
                    .collect()
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                manager
                    .change_dir(&remote_path)
                    .await
                    .map_err(|e| e.to_string())?;
                let files = manager.list_files().await.map_err(|e| e.to_string())?;
                files
                    .iter()
                    .filter(|f| !f.is_dir)
                    .map(|f| (f.name.clone(), f.size.unwrap_or(0)))
                    .collect()
            } else {
                return Err("Not connected to any server".to_string());
            };

            // Compare
            let mut only_local: Vec<Value> = Vec::new();
            let mut only_remote: Vec<Value> = Vec::new();
            let mut size_diff: Vec<Value> = Vec::new();
            let mut identical: Vec<String> = Vec::new();

            for (name, local_size) in &local_files {
                match remote_files.get(name) {
                    Some(&remote_size) if *local_size == remote_size => {
                        identical.push(name.clone());
                    }
                    Some(&remote_size) => {
                        size_diff.push(json!({
                            "name": name,
                            "local_size": local_size,
                            "remote_size": remote_size,
                        }));
                    }
                    None => {
                        only_local.push(json!({ "name": name, "size": local_size }));
                    }
                }
            }
            for (name, remote_size) in &remote_files {
                if !local_files.contains_key(name) {
                    only_remote.push(json!({ "name": name, "size": remote_size }));
                }
            }

            Ok(json!({
                "local_path": local_path,
                "remote_path": remote_path,
                "local_files": local_files.len(),
                "remote_files": remote_files.len(),
                "identical": identical.len(),
                "only_local": only_local,
                "only_remote": only_remote,
                "size_different": size_diff,
                "synced": only_local.is_empty() && only_remote.is_empty() && size_diff.is_empty(),
            }))

        }
        "remote_edit" => {
            guard_no_raw_crypt_write(&state, "remote_edit")?;

            let path = get_str_s(args, "path")?;
            let find = get_str_s(args, "find")?;
            let replace = get_str_s(args, "replace")?;
            let replace_all = args
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(true);
            validate_path(&path, "path")?;

            // Download file content
            let bytes = download_from_provider(&state, &app_state, &path).await?;

            let mut content =
                String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8 text".to_string())?;

            // Strip UTF-8 BOM if present
            content = content
                .strip_prefix('\u{FEFF}')
                .unwrap_or(&content)
                .to_string();

            let occurrences = content.matches(&find).count();
            if occurrences == 0 {
                return Ok(json!({
                    "success": false,
                    "message": "String not found in file",
                    "occurrences": 0,
                }));
            }

            let new_content = if replace_all {
                content.replace(&find, &replace)
            } else {
                content.replacen(&find, &replace, 1)
            };

            // Write back via temp file + upload
            let tmp_path = std::env::temp_dir()
                .join(format!("aeroftp_{}", uuid::Uuid::new_v4()))
                .to_string_lossy()
                .to_string();
            std::fs::write(&tmp_path, &new_content)
                .map_err(|e| format!("Failed to write temp file: {}", e))?;

            let allow_non_atomic = args
                .get("allow_non_atomic")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            // What the publish could not keep (a mode `chmod` refused) is
            // reported as a warning, and this call's own go in its answer:
            // the GUI does not show the process queue.
            let warnings = crate::providers::CallWarnings::default();
            let upload_result = if has_provider(&state).await {
                let mut provider = state.provider.lock().await;
                // No early return: the local temporary is removed below
                // whatever happens here.
                match provider.as_mut() {
                    Some(p) => {
                        let publish =
                            publish_remote_edit(p.as_mut(), &tmp_path, &path, allow_non_atomic);
                        warnings.scope(publish).await
                    }
                    None => Err("No active provider connection".to_string()),
                }
            } else if has_ftp(&app_state).await {
                let mut manager = app_state.ftp_manager.lock().await;
                let publish = publish_remote_edit_on_ftp_manager(
                    &mut manager,
                    &tmp_path,
                    &path,
                    allow_non_atomic,
                );
                warnings.scope(publish).await
            } else {
                Err("Not connected".to_string())
            };

            let _ = std::fs::remove_file(&tmp_path);
            upload_result?;

            let replaced = if replace_all { occurrences } else { 1 };
            let mut answer = json!({
                "success": true,
                "message": format!("Replaced {} occurrence(s) in {}", replaced, path),
                "occurrences": occurrences,
                "replaced": replaced,
            });
            let warnings = warnings.take();
            if !warnings.is_empty() {
                answer["warnings"] = json!(warnings);
            }
            Ok(answer)

        }
        _ => Err(tool_name.to_string()),
        }
    }.await;
    match result {
        Ok(v) => Ok(v),
        Err(e) if e == tool_name => Err(ToolError::NotMigrated(e)),
        Err(e) => Err(ToolError::Exec(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::publish_remote_edit;
    use crate::providers::edit_replace_tests::EditFake;

    fn staged(text: &[u8]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp file");
        std::fs::write(file.path(), text).expect("stage");
        file
    }

    /// A plain FTP server for the legacy session: one folder `/`, whose
    /// files are `(content, mode)` and whose links point elsewhere. Every
    /// command is logged. PASV data connections carry LIST and STOR.
    mod legacy_ftp {
        use std::collections::{BTreeMap, BTreeSet};
        use std::sync::{Arc, Mutex};
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        use tokio::net::TcpListener;

        #[derive(Default)]
        pub struct Remote {
            pub files: BTreeMap<String, (Vec<u8>, u32)>,
            pub links: BTreeMap<String, String>,
            /// Links whose LIST row carries no ` -> ` target, the way a
            /// dangling link arrives.
            pub bare_links: BTreeSet<String>,
            pub log: Vec<String>,
        }

        fn letters(mode: u32) -> String {
            (0..9)
                .map(|bit| {
                    if mode & (0o400 >> bit) == 0 {
                        '-'
                    } else {
                        ['r', 'w', 'x'][bit % 3]
                    }
                })
                .collect()
        }

        pub async fn serve(remote: Arc<Mutex<Remote>>) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap().to_string();
            tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                let mut data: Option<TcpListener> = None;
                let mut from = String::new();
                write.write_all(b"220 ready\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let (cmd, arg) = line.split_once(' ').unwrap_or((line.as_str(), ""));
                    let (cmd, arg) = (cmd.to_uppercase(), arg.trim().to_string());
                    remote.lock().unwrap().log.push(line.clone());
                    let reply = match cmd.as_str() {
                        "USER" => "331 password\r\n".to_string(),
                        "PASS" => "230 in\r\n".to_string(),
                        "PWD" => "257 \"/\"\r\n".to_string(),
                        "CWD" if arg == "/" => "250 ok\r\n".to_string(),
                        "CWD" => "550 no such folder\r\n".to_string(),
                        "PASV" => {
                            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                            let port = listener.local_addr().unwrap().port();
                            data = Some(listener);
                            format!(
                                "227 Entering Passive Mode (127,0,0,1,{},{})\r\n",
                                port / 256,
                                port % 256
                            )
                        }
                        "LIST" => {
                            write.write_all(b"150 listing\r\n").await.unwrap();
                            let (mut socket, _) = data.take().unwrap().accept().await.unwrap();
                            let rows: String = {
                                let r = remote.lock().unwrap();
                                let files = r.files.iter().map(|(p, (c, m))| {
                                    format!(
                                        "-{} 1 u g {} Jan 20 10:00 {}\r\n",
                                        letters(*m),
                                        c.len(),
                                        p.trim_start_matches('/')
                                    )
                                });
                                let links = r.links.iter().map(|(p, t)| {
                                    format!(
                                        "lrwxrwxrwx 1 u g {} Jan 20 10:00 {} -> {}\r\n",
                                        t.len(),
                                        p.trim_start_matches('/'),
                                        t
                                    )
                                });
                                let bare = r.bare_links.iter().map(|p| {
                                    format!(
                                        "lrwxrwxrwx 1 u g 7 Jan 20 10:00 {}\r\n",
                                        p.trim_start_matches('/')
                                    )
                                });
                                files.chain(links).chain(bare).collect()
                            };
                            socket.write_all(rows.as_bytes()).await.unwrap();
                            socket.shutdown().await.unwrap();
                            "226 done\r\n".to_string()
                        }
                        "STOR" => {
                            write.write_all(b"150 send\r\n").await.unwrap();
                            let (mut socket, _) = data.take().unwrap().accept().await.unwrap();
                            let mut body = Vec::new();
                            socket.read_to_end(&mut body).await.unwrap();
                            remote.lock().unwrap().files.insert(arg, (body, 0o644));
                            "226 stored\r\n".to_string()
                        }
                        "SITE" => {
                            let mut words = arg.split_whitespace();
                            let (_, mode, path) = (words.next(), words.next(), words.next());
                            let mode = u32::from_str_radix(mode.unwrap(), 8).unwrap();
                            match remote.lock().unwrap().files.get_mut(path.unwrap()) {
                                Some(file) => {
                                    file.1 = mode;
                                    "200 chmod\r\n".to_string()
                                }
                                None => "550 no such file\r\n".to_string(),
                            }
                        }
                        "RNFR" => {
                            from = arg;
                            "350 go on\r\n".to_string()
                        }
                        "RNTO" => {
                            let mut r = remote.lock().unwrap();
                            match r.files.remove(&from) {
                                Some(file) => {
                                    r.links.remove(&arg);
                                    r.files.insert(arg, file);
                                    "250 renamed\r\n".to_string()
                                }
                                None => "550 no such file\r\n".to_string(),
                            }
                        }
                        "DELE" => {
                            remote.lock().unwrap().files.remove(&arg);
                            "250 deleted\r\n".to_string()
                        }
                        "QUIT" => {
                            let _ = write.write_all(b"221 bye\r\n").await;
                            break;
                        }
                        _ => "200 ok\r\n".to_string(),
                    };
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
            addr
        }
    }

    async fn legacy_session(
        remote: &std::sync::Arc<std::sync::Mutex<legacy_ftp::Remote>>,
    ) -> crate::ftp::FtpManager {
        let addr = legacy_ftp::serve(std::sync::Arc::clone(remote)).await;
        let mut manager = crate::ftp::FtpManager::new();
        manager.connect(&addr).await.unwrap();
        manager.login("u", "p").await.unwrap();
        manager
    }

    /// L17 of the 4.2.1 review: on the legacy FTP session (no provider)
    /// `remote_edit` still stored the new text over the file in place, which
    /// leaves a partial file when the transfer breaks, and dropped the checks
    /// the provider path makes. It now goes through `publish_remote_edit`: a
    /// temporary beside the file, its mode copied, then RNFR/RNTO.
    #[tokio::test]
    async fn remote_edit_on_the_legacy_ftp_session_publishes_a_temporary() {
        let remote = std::sync::Arc::new(std::sync::Mutex::new(legacy_ftp::Remote::default()));
        remote
            .lock()
            .unwrap()
            .files
            .insert("/t.txt".into(), (b"old".to_vec(), 0o600));
        let mut manager = legacy_session(&remote).await;
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();

        super::publish_remote_edit_on_ftp_manager(&mut manager, &local, "/t.txt", false)
            .await
            .expect("edit on the legacy session");

        let r = remote.lock().unwrap();
        assert!(
            !r.log.iter().any(|line| line == "STOR /t.txt"),
            "never a store over the file itself: {:?}",
            r.log
        );
        assert!(
            r.log.iter().any(|line| line == "RNTO /t.txt"),
            "{:?}",
            r.log
        );
        assert_eq!(
            r.files.keys().collect::<Vec<_>>(),
            ["/t.txt"],
            "the temporary is gone"
        );
        assert_eq!(
            r.files["/t.txt"],
            (b"new".to_vec(), 0o600),
            "content and mode"
        );
    }

    /// The replace would put a regular file where the link is: refused
    /// before anything is stored, as on the provider path.
    #[tokio::test]
    async fn remote_edit_on_the_legacy_ftp_session_refuses_a_link() {
        let remote = std::sync::Arc::new(std::sync::Mutex::new(legacy_ftp::Remote::default()));
        remote
            .lock()
            .unwrap()
            .links
            .insert("/t.txt".into(), "real.txt".into());
        let mut manager = legacy_session(&remote).await;
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();

        let outcome =
            super::publish_remote_edit_on_ftp_manager(&mut manager, &local, "/t.txt", false).await;

        let r = remote.lock().unwrap();
        assert!(
            !r.log.iter().any(|line| line.starts_with("STOR")),
            "nothing may be stored over a link: {:?}",
            r.log
        );
        assert!(outcome.unwrap_err().contains("symbolic link"));
    }

    /// The same refusal for a link whose LIST row carries no ` -> ` target:
    /// the adapter derived `is_symlink` from the target, so a dangling link
    /// read as a regular file and the edit staged and replaced through it.
    #[tokio::test]
    async fn remote_edit_on_the_legacy_ftp_session_refuses_a_link_without_a_target() {
        let remote = std::sync::Arc::new(std::sync::Mutex::new(legacy_ftp::Remote::default()));
        remote.lock().unwrap().bare_links.insert("/t.txt".into());
        let mut manager = legacy_session(&remote).await;
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();

        let outcome =
            super::publish_remote_edit_on_ftp_manager(&mut manager, &local, "/t.txt", false).await;

        let r = remote.lock().unwrap();
        assert!(
            !r.log.iter().any(|line| line.starts_with("STOR")),
            "nothing may be stored over a link: {:?}",
            r.log
        );
        assert!(outcome.unwrap_err().contains("symbolic link"));
    }

    /// M2 of the 4.2.1 closeout: the default path asked for an atomic
    /// replace and then uploaded over the file in place, so the answer it
    /// had just checked was never used. It now stages a temporary beside the
    /// file and publishes it with `replace`, as the CLI and MCP edits do.
    #[tokio::test]
    async fn remote_edit_publishes_a_temporary_with_replace() {
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();
        let mut p = EditFake::new(true, false);
        publish_remote_edit(&mut p, &local, "/t.txt", false)
            .await
            .expect("atomic backend");
        assert_eq!(p.uploads.len(), 1, "{:?}", p.uploads);
        assert_ne!(
            p.uploads[0], "/t.txt",
            "never an upload over the file itself"
        );
        assert_eq!(
            p.replaces,
            vec![(p.uploads[0].clone(), "/t.txt".to_string())]
        );
        assert_eq!(p.files.get("/t.txt").map(Vec::as_slice), Some(&b"new"[..]));
        assert_eq!(
            p.files.len(),
            1,
            "the temporary is gone: {:?}",
            p.files.keys()
        );
    }

    /// Without an atomic replace the default refuses before anything is
    /// staged, and names the opt-in only where it can work.
    #[tokio::test]
    async fn remote_edit_refuses_a_backend_without_atomic_replace_before_it_uploads() {
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();
        for sets_aside in [false, true] {
            let mut p = EditFake::new(false, sets_aside);
            let text = publish_remote_edit(&mut p, &local, "/t.txt", false)
                .await
                .unwrap_err();
            assert!(p.uploads.is_empty(), "{:?}", p.uploads);
            assert!(text.contains("Nothing was written"), "{text}");
            assert_eq!(
                text.contains("allow_non_atomic"),
                sets_aside,
                "the opt-in is named exactly where it works: {text}"
            );
            assert_eq!(p.files.get("/t.txt").map(Vec::as_slice), Some(&b"old"[..]));
        }
    }

    /// The opt-in publishes on a set-aside backend and is refused, before
    /// the upload, on one whose replace is its rename.
    #[tokio::test]
    async fn remote_edit_opt_in_is_gated_by_the_set_aside_capability() {
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();

        let mut aside = EditFake::new(false, true);
        publish_remote_edit(&mut aside, &local, "/t.txt", true)
            .await
            .expect("set-aside backend");
        assert_eq!(aside.replaces.len(), 1);
        assert_eq!(
            aside.files.get("/t.txt").map(Vec::as_slice),
            Some(&b"new"[..])
        );

        let mut rename_only = EditFake::new(false, false);
        let text = publish_remote_edit(&mut rename_only, &local, "/t.txt", true)
            .await
            .unwrap_err();
        assert!(
            rename_only.uploads.is_empty() && rename_only.deleted.is_empty(),
            "nothing may be written: uploads {:?}, deleted {:?}",
            rename_only.uploads,
            rename_only.deleted
        );
        assert!(text.contains("Nothing was written"), "{text}");
        assert_eq!(
            rename_only.files.get("/t.txt").map(Vec::as_slice),
            Some(&b"old"[..])
        );
    }

    /// M-A of the 4.2.1 closeout: the upload in place this path made until
    /// 4.2.0 truncated the same file and kept its mode. The replace puts a
    /// new file there, which on SFTP came back with the server default
    /// (a 0600 `.env` became 0644). The mode is copied onto the temporary
    /// before the replace.
    #[tokio::test]
    async fn remote_edit_keeps_the_mode_of_the_file_it_replaces() {
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();
        let mut p = EditFake::new(true, false);
        p.chmod = true;
        p.modes.insert("/t.txt".to_string(), 0o600);

        publish_remote_edit(&mut p, &local, "/t.txt", false)
            .await
            .expect("atomic backend");

        assert_eq!(p.files.get("/t.txt").map(Vec::as_slice), Some(&b"new"[..]));
        assert_eq!(
            p.modes.get("/t.txt").copied(),
            Some(0o600),
            "the edited file must keep its mode, not the server default"
        );
    }

    /// A chmod the server refuses leaves the edit done, and says so.
    #[tokio::test]
    async fn remote_edit_says_which_permissions_it_could_not_keep() {
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();
        let mut p = EditFake::new(true, false);
        p.chmod = true;
        p.chmod_fails_with = Some("permission denied".to_string());
        p.modes.insert("/t.txt".to_string(), 0o600);

        let warnings = crate::providers::CallWarnings::default();
        warnings
            .scope(publish_remote_edit(&mut p, &local, "/t.txt", false))
            .await
            .expect("the edit itself is done");

        assert_eq!(p.files.get("/t.txt").map(Vec::as_slice), Some(&b"new"[..]));
        let taken = warnings.take();
        assert!(
            taken
                .iter()
                .any(|w| w.contains("0600") && w.contains("permission denied")),
            "the warning must name the mode that was not kept and why: {taken:?}"
        );
    }

    /// The replace would put a regular file in the link's place and leave
    /// the file it points to unchanged: a link is refused before anything
    /// is staged, and the refusal names the path to edit instead.
    #[tokio::test]
    async fn remote_edit_refuses_a_symlink_before_it_uploads() {
        let local = staged(b"new");
        let local = local.path().to_string_lossy().to_string();
        let mut p = EditFake::new(true, false);
        p.links.insert("/t.txt".to_string(), "real.txt".to_string());

        let outcome = publish_remote_edit(&mut p, &local, "/t.txt", false).await;

        assert!(
            p.uploads.is_empty() && p.replaces.is_empty(),
            "nothing may be staged over a link: {outcome:?}"
        );
        let text = outcome.unwrap_err();
        assert!(
            text.contains("symbolic link") && text.contains("/real.txt"),
            "{text}"
        );
        assert_eq!(p.files.get("/t.txt").map(Vec::as_slice), Some(&b"old"[..]));
    }
}
