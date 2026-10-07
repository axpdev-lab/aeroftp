// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { getUnlockStatus } from '../utils/userPartitions';
import { GuiError } from './errors';
import type { ConnectScope } from './connectScope';

/** Capture account identity before queueing; recheck it at every awaited step. */
export async function bindSettingsScope(parent: ConnectScope): Promise<ConnectScope> {
    const initial = await parent.step(getUnlockStatus);
    if (!initial.isUnlocked) throw new GuiError('locked');
    return parent.checked(async () => {
        const current = await getUnlockStatus();
        if (!current.isUnlocked || current.activeUserId !== initial.activeUserId ||
            current.unlockedUserId !== initial.unlockedUserId) throw new GuiError('lease_interrupted');
    });
}
