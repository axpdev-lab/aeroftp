// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';
import { getUnlockStatus } from '../utils/userPartitions';
import { GuiError } from './errors';
import type { ConnectScope } from './connectScope';

/** Capture account identity before queueing; recheck it at every awaited step. */
async function checkBroker(id: string): Promise<void> {
    try { await invoke('gui_intent_check', { id }); } catch { throw new GuiError('lease_interrupted'); }
}
export async function bindSettingsScope(parent: ConnectScope): Promise<ConnectScope> {
    const initial = await parent.step(getUnlockStatus);
    if (!initial.isUnlocked) throw new GuiError('locked');
    if (parent.brokerId) await parent.step(() => checkBroker(parent.brokerId!));
    return parent.checked(async () => {
        if (parent.brokerId) await checkBroker(parent.brokerId);
        const current = await getUnlockStatus();
        if (!current.isUnlocked || current.activeUserId !== initial.activeUserId ||
            current.unlockedUserId !== initial.unlockedUserId) throw new GuiError('lease_interrupted');
    });
}
