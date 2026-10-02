// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';
import libRs from '../../src-tauri/src/lib.rs?raw';

/**
 * Every Tauri command registered in `lib.rs` has a caller in the frontend.
 *
 * A registered command is IPC surface: anything running in the webview can
 * invoke it, whether or not the app ever does. A command nobody calls costs
 * that surface and buys nothing, and it rots silently because no flow
 * exercises it. Seven `user_partitions_*` commands were found in that state,
 * one of which handed a decrypted per-user credential to the webview as a
 * plain string; all of them were superseded by flows that never needed them.
 *
 * "Has a caller" follows one hop. A command name that appears only inside a
 * wrapper (`export async function getActiveUser() { invoke(...) }`) is not
 * called by being wrapped: four of the seven had a typed wrapper that nothing
 * imported. So a literal inside a named top-level declaration counts only when
 * that name is used again: elsewhere in its own file, or, when the file exports
 * it (inline or through an `export { ... }` list), under its exported name in
 * another file. The export list itself is not a use. A literal outside any
 * named declaration (a top-level call, an anonymous default export) counts as
 * a caller as it stands.
 *
 * `INHERITED_UNCALLED` is the set that already had no caller when this guard
 * landed and has not been audited yet. It may only shrink: an entry that gains
 * a caller or is unregistered fails the last test until it is removed here.
 */

const sources = import.meta.glob('../**/*.{ts,tsx}', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/**
 * Command names from the `tauri::generate_handler![` block, parsed the way
 * `src-tauri/build.rs` parses it for the inventory: last path segment is the
 * name, attributes and comments skipped. `cargo fmt` keeps one path per line;
 * a line holding several is split on commas rather than read as one name, and
 * every name is checked to be an identifier by the first test below.
 */
function registeredCommands(lib: string): string[] {
    const lines = lib.split('\n');
    const start = lines.findIndex((l) => l.includes('generate_handler!['));
    const names: string[] = [];
    if (start < 0) return names;
    for (const raw of lines.slice(start + 1)) {
        const line = raw.trim();
        if (line.startsWith('])')) break;
        if (line === '' || line.startsWith('#[') || line.startsWith('//')) continue;
        const end = line.indexOf('])');
        for (const part of (end >= 0 ? line.slice(0, end) : line).split(',')) {
            const token = part.trim();
            if (token === '' || token.startsWith('//')) continue;
            names.push(token.split('::').pop() ?? token);
        }
        if (end >= 0) break;
    }
    return names;
}

// The scan reads the AST, not the text. A regex that blanks comments cannot
// tell a `/**` inside a line comment from the start of a block comment, and the
// first version of this guard lost a hundred lines of `useUiTokens.ts` to that.
type Node = { type: string; [key: string]: unknown };

function children(node: Node): Node[] {
    const out: Node[] = [];
    for (const value of Object.values(node)) {
        if (Array.isArray(value)) {
            for (const item of value) if (item && typeof item === 'object' && 'type' in item) out.push(item as Node);
        } else if (value && typeof value === 'object' && 'type' in value) {
            out.push(value as Node);
        }
    }
    return out;
}

/** The name a top-level statement declares (through an inline export too), or null. */
function declaredName(statement: Node): string | null {
    const isExport = statement.type === 'ExportNamedDeclaration' || statement.type === 'ExportDefaultDeclaration';
    const decl = (isExport ? statement.declaration : statement) as Node | null;
    if (!decl) return null;
    const id = decl.id as { name?: string } | null | undefined;
    if (id?.name) return id.name;
    const declarators = decl.declarations as Array<{ id: { type: string; name?: string } }> | undefined;
    if (declarators?.length === 1 && declarators[0].id.type === 'Identifier') return declarators[0].id.name ?? null;
    return null;
}

/** Local name -> the names other files can import it by. */
function exportedNames(program: Node): Map<string, Set<string>> {
    const exported = new Map<string, Set<string>>();
    const add = (local: string, name: string) => exported.set(local, (exported.get(local) ?? new Set()).add(name));
    for (const statement of program.body as Node[]) {
        if (statement.type === 'ExportNamedDeclaration' || statement.type === 'ExportDefaultDeclaration') {
            const name = declaredName(statement);
            if (name !== null) add(name, name);
        }
        // `export { a, b as c }` without `from`: names declared in this file.
        if (statement.type === 'ExportNamedDeclaration' && !statement.source) {
            for (const spec of statement.specifiers as Array<{ local: { name?: string }; exported: { name?: string } }>) {
                if (spec.local.name && spec.exported.name) add(spec.local.name, spec.exported.name);
            }
        }
    }
    return exported;
}

/** The string a literal node spells, for plain strings and substitution-free templates. */
function stringValue(node: Node): string | null {
    if (node.type === 'Literal' && typeof node.value === 'string') return node.value;
    if (node.type === 'TemplateLiteral' && (node.expressions as unknown[]).length === 0) {
        const quasis = node.quasis as Array<{ value: { cooked: string | null } }>;
        return quasis[0]?.value.cooked ?? null;
    }
    return null;
}

interface ScannedFile {
    file: string;
    /** Every string literal in the file, with the named top-level declaration it sits in. */
    strings: Array<{ value: string; wrapper: string | null }>;
    /** How many times each identifier (including JSX tag names) occurs, export lists excluded. */
    identifiers: Map<string, number>;
    exported: Map<string, Set<string>>;
}

function scan(file: string, source: string): ScannedFile {
    const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
    const scanned: ScannedFile = { file, strings: [], identifiers: new Map(), exported: exportedNames(program) };
    for (const statement of program.body as Node[]) {
        const wrapper = declaredName(statement);
        const stack: Node[] = [statement];
        while (stack.length > 0) {
            const node = stack.pop() as Node;
            // `export { unused }` names a binding, it does not use it.
            if (node.type === 'ExportSpecifier') continue;
            const value = stringValue(node);
            if (value !== null) scanned.strings.push({ value, wrapper });
            if (node.type === 'Identifier' || node.type === 'JSXIdentifier') {
                const name = node.name as string;
                scanned.identifiers.set(name, (scanned.identifiers.get(name) ?? 0) + 1);
            }
            stack.push(...children(node));
        }
    }
    return scanned;
}

/** Test and type files are not callers: only code the app ships can invoke a command. */
function isAppSource(file: string): boolean {
    return !/\.(?:test|spec)\.tsx?$/.test(file) && !file.includes('/__tests__/') && !file.endsWith('.d.ts');
}

const appFiles = Object.entries(sources)
    .filter(([file]) => isAppSource(file))
    .map(([file, source]) => scan(file, source));

/** Whether a named top-level declaration of `scanned` is used anywhere but its own declaration. */
function isUsed(scanned: ScannedFile, wrapper: string, files: ScannedFile[]): boolean {
    // The declaration's own name is one occurrence; any other in the file is a use.
    if ((scanned.identifiers.get(wrapper) ?? 0) > 1) return true;
    const names = scanned.exported.get(wrapper);
    if (!names) return false;
    return files.some((other) => other !== scanned && [...names].some((n) => (other.identifiers.get(n) ?? 0) > 0));
}

/** Where a command is called from, following one hop through a named wrapper. */
function callersOf(command: string, files: ScannedFile[] = appFiles): string[] {
    const callers: string[] = [];
    for (const scanned of files) {
        for (const { value, wrapper } of scanned.strings) {
            if (value !== command) continue;
            if (wrapper === null) callers.push(scanned.file);
            else if (isUsed(scanned, wrapper, files)) callers.push(`${scanned.file} via ${wrapper}`);
        }
    }
    return callers;
}

/**
 * Registered with no frontend caller when this guard landed (2026-10-02,
 * origin/main 5a7efa640), not yet audited one by one. Each entry is either
 * called by a name the scan cannot see (a computed `invoke(\`${x}_...\`)`) or
 * dead surface waiting for the same treatment the `user_partitions_*` seven got.
 */
const INHERITED_UNCALLED: string[] = [
    'agent_memory_delete',
    'ai_execute_tool',
    'app_master_password_status',
    'azure_set_blob_tier',
    'box_add_collaboration',
    'box_add_comment',
    'box_delete_comment',
    'box_list_collaborations',
    'box_list_comments',
    'box_list_folder_locks',
    'box_move_file',
    'box_remove_collaboration',
    'box_unlock_folder',
    'chat_history_delete_sessions_bulk',
    'chat_history_export_session',
    'chat_history_import',
    'chat_history_init',
    'check_connection',
    'clear_file_badge',
    'debug_panic_command',
    'deepseek_fim_complete',
    'delete_sync_profile_cmd',
    'delta_sync_analyze',
    'detect_renames_cmd',
    'dropbox_get_tags',
    'dropbox_set_tags',
    'enable_aerocloud',
    'extract_7z_entry',
    'extract_rar_entry',
    'extract_tar_entry',
    'extract_zip_entry',
    'file_tags_delete_all_for_file',
    'file_tags_get_files_by_label',
    'file_tags_update_path',
    'filelu_restore_folder',
    'fourshared_complete_auth',
    'fourshared_start_auth',
    'gemini_create_cache',
    'get_badge_status',
    'get_compare_options_default',
    'get_default_retry_policy',
    'get_parallel_scan_files',
    'get_speed_limit',
    'github_batch_commit',
    'github_get_release',
    'gitlab_get_web_url',
    'gitlab_switch_branch',
    'google_drive_delete_comment',
    'google_drive_list_comments',
    'google_drive_set_description',
    'google_drive_set_properties',
    'google_drive_trash_file',
    'install_plugin',
    'is_running_as_snap',
    'jottacloud_move_to_trash',
    'kimi_create_cache',
    'kimi_upload_file',
    'load_sync_snapshot_cmd',
    'mega_move_to_trash',
    'mtp_backend_status',
    'native_rsync_enabled_get',
    'native_rsync_enabled_set',
    'oauth2_start_auth',
    'onedrive_trash_files',
    'parallel_sync_execute',
    'peer_receiver_status',
    'peer_send_action',
    'provider_check_connection',
    'provider_disk_usage',
    'provider_exists',
    'provider_file_size',
    'provider_get_speed_limit',
    'provider_go_up',
    'provider_pwd',
    'provider_resume_download',
    'provider_resume_upload',
    'provider_server_info',
    'provider_share_link_capabilities',
    'provider_stat',
    'provider_supports_resume',
    'provider_supports_server_copy',
    'rclone_crypt_decrypt_file',
    'rclone_crypt_encrypt_file_path',
    'rclone_crypt_encrypt_name',
    'read_agent_memory',
    'read_export_metadata',
    'rebuild_menu',
    's3_change_storage_class',
    's3_delete_object_tags',
    's3_get_object_tags',
    's3_glacier_restore',
    's3_set_object_tags',
    'session_change_dir',
    'session_connect',
    'session_create_share_link',
    'session_delete',
    'session_disconnect',
    'session_download',
    'session_info',
    'session_list',
    'session_list_files',
    'session_mkdir',
    'session_rename',
    'session_switch',
    'session_upload',
    'set_file_badge',
    'sign_sync_journal',
    'speedtest_history_clear',
    'speedtest_history_list',
    'start_badge_server_cmd',
    'stop_badge_server_cmd',
    'sync_canary_approve',
    'totp_load_secret',
    'totp_verify',
    'transfer_queue_scan_remote_tree',
    'trigger_plugin_hooks',
    'update_cloud_pair',
    'update_conflict_strategy',
    'update_tray_badge_cmd',
    'vault_mount_list',
    'vault_v2_compact',
    'vault_v2_copy_entry',
    'vault_v2_move_entry',
    'vault_v2_rename_entry',
    'vault_v2_security_info',
    'vault_v3_copy_entry',
    'vault_v3_has_error_correction',
    'vault_v3_move_entry',
    'vault_v3_rename_entry',
    'vault_v3_security_info',
    'write_agent_memory',
    'zoho_add_file_label',
    'zoho_create_label',
    'zoho_get_file_labels',
    'zoho_get_user_info',
    'zoho_list_team_labels',
    'zoho_remove_file_label',
];

const registered = registeredCommands(libRs);
const uncalled = registered.filter((c) => callersOf(c).length === 0);

describe('registered Tauri commands have a frontend caller', () => {
    it('parses a command list large enough to be the real handler block', () => {
        // A parser that silently matched nothing would make every assertion below pass.
        expect(registered.length).toBeGreaterThan(500);
        expect(appFiles.length).toBeGreaterThan(300);
        // A mis-split line yields a name no command has, which would then read as "uncalled".
        expect(registered.filter((name) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name))).toEqual([]);
    });

    it('reads a handler line holding several paths as several commands', () => {
        const lib = 'x.invoke_handler(tauri::generate_handler![\n    a::one, b::two,\n    three])\n';
        expect(registeredCommands(lib)).toEqual(['one', 'two', 'three']);
    });

    it('does not count a wrapper that is only listed in an export block', () => {
        const wrapperFile = scan(
            'src/fake/wrappers.ts',
            "const unused = () => invoke('fake_cmd');\nconst used = () => invoke('fake_used');\nexport { unused, used as renamed };\n",
        );
        const consumer = scan('src/fake/consumer.ts', "import { renamed } from './wrappers';\nrenamed();\n");
        expect(callersOf('fake_cmd', [wrapperFile, consumer])).toEqual([]);
        expect(callersOf('fake_used', [wrapperFile, consumer])).toEqual(['src/fake/wrappers.ts via used']);
    });

    it('does not count a literal parked in a constant nobody reads', () => {
        const file = scan('src/fake/parked.ts', "const PARKED = 'fake_cmd';\n");
        expect(callersOf('fake_cmd', [file])).toEqual([]);
    });

    it('excludes spec files from the callers', () => {
        expect(isAppSource('src/utils/thing.spec.ts')).toBe(false);
        expect(isAppSource('src/utils/thing.spec.tsx')).toBe(false);
        expect(isAppSource('src/utils/thing.ts')).toBe(true);
    });

    it('follows a wrapper to its callers instead of counting the wrapper itself', () => {
        // `listUsers` wraps `user_partitions_list_users` and is imported across the app.
        expect(callersOf('user_partitions_list_users').some((c) => c.endsWith('via listUsers'))).toBe(true);
    });

    it('registers no new command that nothing in src/ invokes', () => {
        const inherited = new Set(INHERITED_UNCALLED);
        expect(uncalled.filter((c) => !inherited.has(c))).toEqual([]);
    });

    it('keeps the inherited list exact: it only shrinks', () => {
        const stale = INHERITED_UNCALLED.filter((c) => !uncalled.includes(c));
        expect(stale, 'now called or no longer registered: drop it from INHERITED_UNCALLED').toEqual([]);
    });
});
