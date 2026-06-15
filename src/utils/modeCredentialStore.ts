import { invoke } from '@tauri-apps/api/core';
import type { ConnectionParams } from '../types';

/**
 * Issue #215: persistent per-mode credential snapshots.
 *
 * A single saved profile can expose several protocol "modes" of one online
 * account (Filen API / Local WebDAV / Local S3, MEGA API / MEGAcmd / WebDAV /
 * S4, ...). The in-memory snapshot map in ConnectionScreen keeps each mode's
 * typed credentials alive while switching tabs in one edit session. When the
 * user opts in (ServerProfile.persistModeCredentials), that map is also written
 * to the encrypted vault so the credentials survive a restart and the user
 * never re-enters them: one profile per account, switch protocols freely.
 *
 * Storage lives under `server_modes_<profileId>`. The `server_` prefix means
 * the vault dual-write routes it through the active user's partition exactly
 * like the per-profile password, so the secrets stay per-user isolated (MUV).
 */
export interface ModeCredentialSnapshot {
    username: string;
    password: string;
    server: string;
    port?: number;
    options?: ConnectionParams['options'];
}

export type ModeCredentialMap = Record<string, ModeCredentialSnapshot>;

const accountFor = (profileId: string) => `server_modes_${profileId}`;

/**
 * Single-use secrets must never be persisted: a TOTP/STS code rotates and
 * reusing yesterday's value tomorrow always fails (mirrors the saved-profile
 * policy in saveToServers). The long-lived secrets (password, S3 keys,
 * filen_api_key, totp_secret) are kept so the restored tab is ready to connect.
 */
const stripSingleUseSecrets = (map: ModeCredentialMap): ModeCredentialMap => {
    const cleaned: ModeCredentialMap = {};
    for (const [key, snap] of Object.entries(map)) {
        if (snap.options) {
            const opts = { ...snap.options };
            delete opts.two_factor_code;
            delete opts.roleMfaTokenCode;
            cleaned[key] = { ...snap, options: opts };
        } else {
            cleaned[key] = snap;
        }
    }
    return cleaned;
};

export async function loadModeCredentials(profileId: string): Promise<ModeCredentialMap> {
    try {
        const json = await invoke<string>('get_credential', { account: accountFor(profileId) });
        if (!json) return {};
        const parsed = JSON.parse(json);
        return parsed && typeof parsed === 'object' ? (parsed as ModeCredentialMap) : {};
    } catch {
        // Not stored (or vault not ready): no persisted modes, fall back to the
        // in-session snapshots.
        return {};
    }
}

export async function storeModeCredentials(profileId: string, map: ModeCredentialMap): Promise<void> {
    try {
        await invoke('store_credential', {
            account: accountFor(profileId),
            password: JSON.stringify(stripSingleUseSecrets(map)),
        });
    } catch (err) {
        console.error('Failed to persist per-mode credentials:', err);
    }
}

export async function deleteModeCredentials(profileId: string): Promise<void> {
    try {
        await invoke('delete_credential', { account: accountFor(profileId) });
    } catch {
        // Already absent is fine.
    }
}
