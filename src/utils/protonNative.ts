// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

/** What `proton_native_status` answers for a profile. */
export type ProtonNativeStatus = { available: boolean; signedIn: boolean };

let availability: Promise<boolean> | null = null;

/**
 * Whether this build includes the native Proton Drive client. Its commands
 * exist only in builds with the `proton-native` feature, so a status call
 * that is refused (command not found) means no native client. Asked once
 * per run.
 */
export function protonNativeAvailable(): Promise<boolean> {
    availability ??= invoke<ProtonNativeStatus | null>('proton_native_status', { profileId: null })
        .then((status) => status?.available === true)
        .catch(() => false);
    return availability;
}

/** {@link protonNativeAvailable} for a component: false until answered. */
export function useProtonNativeAvailable(): boolean {
    const [available, setAvailable] = useState(false);
    useEffect(() => {
        let live = true;
        protonNativeAvailable().then((answer) => {
            if (live) setAvailable(answer);
        });
        return () => {
            live = false;
        };
    }, []);
    return available;
}

/** Whether a connection's options select the native client. */
export function isProtonNative(options?: { proton_mode?: string | null } | null): boolean {
    return options?.proton_mode === 'native';
}
