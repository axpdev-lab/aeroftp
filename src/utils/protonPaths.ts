// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { isProtonNative } from './protonNative';

/**
 * Root of a used-storage scan of the connected remote. The saved profile's
 * initial folder when it has one, else the current folder, as for every
 * provider, except the scan a Proton Drive profile on the CLI runs by itself
 * on connect: its sessions now start in My Files (Requested 79), and a saved
 * folder is under My Files too, while the account's storage spans the
 * sections the CLI lists at `/`, the root that scan used to start from.
 */
export function usedScanRoot(args: {
    currentRemotePath: string | null | undefined;
    automatic: boolean;
    protocol?: string | null;
    options?: { proton_mode?: string | null } | null;
    /** The saved profile's initial folder, resolved. */
    profileRoot?: string | null;
}): string {
    if (args.automatic && args.protocol === 'proton' && !isProtonNative(args.options)) {
        return '/';
    }
    return args.profileRoot || args.currentRemotePath || '/';
}
