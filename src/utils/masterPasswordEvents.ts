// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export const MASTER_PASSWORD_CHANGED_EVENT = 'aeroftp-master-password-changed';

export interface MasterPasswordChangedDetail {
    enabled: boolean;
    isLocked?: boolean;
    timeoutSeconds?: number;
}

export const dispatchMasterPasswordChanged = (detail: MasterPasswordChangedDetail): void => {
    try {
        window.dispatchEvent(new CustomEvent<MasterPasswordChangedDetail>(
            MASTER_PASSWORD_CHANGED_EVENT,
            { detail },
        ));
    } catch {
        // Browserless tests: best effort.
    }
};
