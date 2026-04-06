//! MCP prompt templates
//!
//! 4 reusable prompt templates for common file transfer workflows:
//! - deploy_files — upload build artifacts to server
//! - backup_remote — download remote directory as backup
//! - sync_directories — synchronize local and remote directories
//! - find_and_clean — find and remove old/large files

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

use serde_json::{json, Value};

/// Prompt definition for `prompts/list`.
pub struct McpPromptDef {
    pub name: &'static str,
    pub description: &'static str,
    pub arguments: Vec<PromptArg>,
}

pub struct PromptArg {
    pub name: &'static str,
    pub description: &'static str,
    pub required: bool,
}

/// Get all prompt definitions.
pub fn prompt_definitions() -> Vec<McpPromptDef> {
    vec![
        McpPromptDef {
            name: "deploy_files",
            description: "Upload build artifacts from a local directory to a remote server",
            arguments: vec![
                PromptArg {
                    name: "server",
                    description: "Target server name",
                    required: true,
                },
                PromptArg {
                    name: "local_dir",
                    description: "Local build directory (e.g. ./dist)",
                    required: true,
                },
                PromptArg {
                    name: "remote_dir",
                    description: "Remote deployment directory",
                    required: true,
                },
            ],
        },
        McpPromptDef {
            name: "backup_remote",
            description: "Download a remote directory as a local backup",
            arguments: vec![
                PromptArg {
                    name: "server",
                    description: "Source server name",
                    required: true,
                },
                PromptArg {
                    name: "remote_dir",
                    description: "Remote directory to back up",
                    required: true,
                },
                PromptArg {
                    name: "local_dir",
                    description: "Local backup destination (default: ./backup)",
                    required: false,
                },
            ],
        },
        McpPromptDef {
            name: "sync_directories",
            description: "Synchronize a local directory with a remote directory",
            arguments: vec![
                PromptArg {
                    name: "server",
                    description: "Target server name",
                    required: true,
                },
                PromptArg {
                    name: "local_dir",
                    description: "Local source directory",
                    required: true,
                },
                PromptArg {
                    name: "remote_dir",
                    description: "Remote target directory",
                    required: true,
                },
            ],
        },
        McpPromptDef {
            name: "find_and_clean",
            description: "Find and remove old or large files on a remote server",
            arguments: vec![
                PromptArg {
                    name: "server",
                    description: "Target server name",
                    required: true,
                },
                PromptArg {
                    name: "path",
                    description: "Remote path to scan (default: /)",
                    required: false,
                },
                PromptArg {
                    name: "pattern",
                    description: "File pattern to match (e.g. \"*.log\", \"*.tmp\")",
                    required: false,
                },
            ],
        },
    ]
}

/// Format prompt definitions for `prompts/list` response.
pub fn prompts_list() -> Vec<Value> {
    prompt_definitions()
        .iter()
        .map(|p| {
            let args: Vec<Value> = p
                .arguments
                .iter()
                .map(|a| {
                    json!({
                        "name": a.name,
                        "description": a.description,
                        "required": a.required,
                    })
                })
                .collect();
            json!({
                "name": p.name,
                "description": p.description,
                "arguments": args,
            })
        })
        .collect()
}

/// Get a prompt by name, filling in argument values.
/// Returns `Some(messages)` or `None` if prompt not found.
pub fn get_prompt(name: &str, args: &Value) -> Option<Vec<Value>> {
    let get_arg = |key: &str, default: &str| -> String {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or(default)
            .to_string()
    };

    match name {
        "deploy_files" => {
            let server = get_arg("server", "");
            let local_dir = get_arg("local_dir", "./dist");
            let remote_dir = get_arg("remote_dir", "/");
            Some(vec![json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!(
                        "Deploy files from local directory '{}' to remote server '{}' at path '{}'.\n\n\
                         Steps:\n\
                         1. Use aeroftp_list_servers to verify the server exists\n\
                         2. Use aeroftp_list_files to check the remote directory\n\
                         3. Upload each file from the local directory using aeroftp_upload_file\n\
                         4. Verify the upload by listing the remote directory again\n\n\
                         Important: Create remote directories first if they don't exist.",
                        local_dir, server, remote_dir
                    )
                }
            })])
        }

        "backup_remote" => {
            let server = get_arg("server", "");
            let remote_dir = get_arg("remote_dir", "/");
            let local_dir = get_arg("local_dir", "./backup");
            Some(vec![json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!(
                        "Create a backup of remote directory '{}' on server '{}' to local path '{}'.\n\n\
                         Steps:\n\
                         1. Use aeroftp_list_files to enumerate all files in the remote directory\n\
                         2. Download each file using aeroftp_download_file, preserving directory structure\n\
                         3. Report summary: total files, total size, any errors\n\n\
                         Important: Skip files larger than 50 MB and report them separately.",
                        remote_dir, server, local_dir
                    )
                }
            })])
        }

        "sync_directories" => {
            let server = get_arg("server", "");
            let local_dir = get_arg("local_dir", ".");
            let remote_dir = get_arg("remote_dir", "/");
            Some(vec![json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!(
                        "Synchronize local directory '{}' with remote directory '{}' on server '{}'.\n\n\
                         Steps:\n\
                         1. List both local and remote directories\n\
                         2. Compare file names and sizes to find differences\n\
                         3. Upload files that are new or modified locally\n\
                         4. Report what was uploaded and what was already in sync\n\n\
                         Important: Do NOT delete remote files that don't exist locally. \
                         Only add/update files. Ask before overwriting if a remote file is newer.",
                        local_dir, remote_dir, server
                    )
                }
            })])
        }

        "find_and_clean" => {
            let server = get_arg("server", "");
            let path = get_arg("path", "/");
            let pattern = get_arg("pattern", "*.log");
            Some(vec![json!({
                "role": "user",
                "content": {
                    "type": "text",
                    "text": format!(
                        "Find and clean up files matching '{}' on server '{}' under path '{}'.\n\n\
                         Steps:\n\
                         1. Use aeroftp_search_files to find matching files\n\
                         2. Use aeroftp_file_info to check size and age of each file\n\
                         3. Present the list with sizes, sorted by size descending\n\
                         4. Ask for confirmation before deleting anything\n\
                         5. Use aeroftp_delete only after explicit confirmation\n\n\
                         Important: NEVER delete without asking. Show total space to be freed.",
                        pattern, server, path
                    )
                }
            })])
        }

        _ => None,
    }
}
