# MCP managed installation and directory permissions

The STDIO settings boundary supports explicit read-only directory grants and a backend-owned managed installer. Existing server settings deserialize to no directories and no network access.

## Trust and permissions

Only the main window can change permissions or install a server. Generic CLI and renderer partition-setting APIs reserve every `aeroagent_mcp_` scope, including reads, writes and deletion, so MCP catalogs remain behind their dedicated guarded commands. Ordinary server edits preserve the sandbox descriptor; they cannot create grants, change network consent, or replace a managed command or entry point. Permission changes increment the stored revision. The effective revision includes the complete descriptor, user identity and vault-backed environment secrets, so previous snapshots and tool approvals cannot authorize the changed configuration.

Custom servers can request up to four canonical, non-overlapping directories. The user chooses each directory and confirms access to all its files. System roots, home roots, credential/application directories, aliases, symbolic links, hard links and special files are refused. Renew access confirms one selected path again; adding a different grant cannot silently renew another directory that was replaced. Directory identity is recorded as device/inode strings, preserving precision through the renderer. Custom servers have no network permission.

Managed manifests can declare network access. A separate recorded user consent enables the host network namespace, including local services; this is not a host allowlist. The confirmation states this scope. Removing consent restores network isolation. Additional project-directory permissions for curated servers belong to their reviewed preset contract.

## Reviewed manifest contract

`src-tauri/src/mcp_client_install.rs` owns the manifest registry. The renderer supplies only a manifest ID/version, an operation UUID and consent, never an archive URL, hash, runtime command or manifest body. The STDIO registry is still empty: a curated STDIO preset ships only with a reviewed download pin (see Recommended servers). Generic installer controls load the registry, confirm declared network access, and offer scoped cancellation while an install is pending; an empty registry adds no visible cards. Closing settings cancels an unfinished install.

A STDIO manifest identifies:

- immutable ID and version;
- a public HTTPS archive URL and SHA-256 of the exact compressed bytes;
- the system Node or Python runtime;
- a relative entry file and literal arguments;
- whether network access is required.

The artifact is a complete runnable **tar.gz** tree with all transitive dependencies already pinned and included. Paths are relative to the archive root. Only ordinary files and directories are accepted. Absolute paths, traversal, Windows path spellings, duplicate entries, links, device files, FIFOs and PAX/GNU metadata entries are refused. Permissions, ownership and executable modes from the archive are not applied. Python artifacts use the system interpreter and include dependencies without a symlink-based virtualenv. No package install scripts or package managers execute, and no dependency download occurs at launch.

Limits: 64 MiB compressed download, 256 MiB expanded tree, 8,192 entries, 32 directory levels and 1,024-byte relative paths. HTTPS requests reuse the MCP transport's public-address DNS pinning, disabled proxies and refused redirects. Installation has a 120-second outer download deadline plus the transport's request deadline. Staging and directory copying check cancellation between entries and chunks.

## Recommended servers

AI Settings > MCP shows a Recommended servers section with reviewed presets. The first one is DeepWiki, an HTTP preset: `src-tauri/src/mcp_client_presets.rs` holds its fixed endpoint `https://mcp.deepwiki.com/mcp`, and the renderer sends only the preset id. Install adds it to the HTTP server list with no authentication and leaves it disabled until the user turns it on; an existing id returns `MCP_INSTALL_EXISTS`. Removing it from the HTTP list brings the Install button back.

Curated STDIO presets (the Fetch and Git reference servers) are not shipped yet. They will be downloaded at install time from a pinned HTTPS URL, verified against a SHA-256 in the manifest, and offered only on the platforms and architectures their native dependencies support. Nothing is embedded in the AeroFTP binary.

## Storage, launch and removal

Artifacts are scoped to the active user under the app data directory:

```text
mcp-servers/user-<user-id>/<manifest-id>/<version>/
```

The installer verifies the archive hash, extracts into a private same-filesystem staging directory, hashes the resulting tree, then publishes with an atomic no-replace rename. The catalog writes the pinned artifact identity and tree digest in its existing immediate transaction. A failed catalog write removes the newly published artifact. Installs start disabled. Initial installation uses expected revision `0`; existing installations must be removed before reinstalling. Version updates are a separate reviewed preset workflow.

Each directory launch creates a bounded, link-free private snapshot through pinned directory/file descriptors. Managed snapshots must match the installed tree digest. Bubblewrap mounts the snapshot by file descriptor read-only at the granted path. The launch command retains the snapshot for the child lifetime, so changing the original path during launch cannot substitute a different tree. Copying and hashing run on a blocking worker; cancellation or a dropped discovery future discards the preparation, and freshness is checked again before launch.

Processes keep cleared environments, private `/tmp`, namespaces, `die-with-parent`, new sessions, cancellation and kill-on-drop. Production Windows and macOS launches and permission/install commands remain unavailable until equivalent isolation exists. Linux requires bubblewrap's `--ro-bind-fd` support for directory grants; an unsupported host fails closed.

Remove moves the managed tree into a private quarantine while its catalog transaction is pending. Rollback restores it. After commit the quarantined tree and the server's vault references are removed. Custom-directory revocation never deletes the selected host directory. Installer operations are scoped to the user, exclusive per user, cancellable by operation UUID, and cancelled when the vault/session is cleared. A bounded, expiring pre-start cancellation record prevents Cancel from being acknowledged before registration and then ignored. A lifecycle generation check also covers a vault/session clear during initial context resolution.
