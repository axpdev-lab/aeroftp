// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

type Invoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
export type ProfileCredentialWriter = (account: string, password: string | null) => Promise<void>;

/** Keep only the credentials this save changes, in memory until persistence. */
export function createProfileCredentialJournal(invoke: Invoke) {
    const before = new Map<string, string | null>();
    let failure: unknown;
    return {
        write: async (account: string, password: string | null): Promise<void> => {
            try {
                if (!before.has(account)) {
                    try {
                        before.set(account, await invoke<string>('get_credential', { account }));
                    } catch (err) {
                        // Missing and unavailable are distinct. Never mutate a
                        // secret whose previous value could not be read.
                        if (!String(err).includes('Credential not found:')) throw err;
                        before.set(account, null);
                    }
                }
                if (password === null) await invoke('delete_credential', { account });
                else await invoke('store_credential', { account, password });
            } catch (err) {
                failure = err;
                throw err;
            }
        },
        assertReady: () => { if (failure !== undefined) throw failure; },
        commit: () => { before.clear(); },
        rollback: async () => {
            const failed: string[] = [];
            for (const [account, password] of before) {
                try {
                    if (password === null) await invoke('delete_credential', { account });
                    else await invoke('store_credential', { account, password });
                } catch {
                    failed.push(account);
                }
            }
            before.clear();
            if (failed.length) throw new Error(`Could not restore saved profile credentials: ${failed.join(', ')}`);
        },
    };
}

export type ProfileCredentialJournal = ReturnType<typeof createProfileCredentialJournal>;
