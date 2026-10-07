// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ServerProfile } from '../types';
import { getUnlockStatus } from '../utils/userPartitions';
import { loadSavedServerProfilesStrict } from '../utils/serverProfileStore';
import { GuiError } from './controller';
import type { ConnectScope, ProfileConnector, ProfileConnectOutcome } from './connectScope';

/** Resolve only exact, uniquely owned IDs. Profile contents stay inside the UI flow. */
export function createProfileConnector(
    connect: (profile: ServerProfile, scope: ConnectScope) => Promise<ProfileConnectOutcome>,
    mounted: () => boolean,
): ProfileConnector {
    return async (profileId, parent) => {
        const initial = await parent.step(() => getUnlockStatus());
        if (!initial.isUnlocked) throw new GuiError('locked');
        const scope = parent.checked(async () => {
            if (!mounted()) throw new GuiError('lease_interrupted');
            const current = await getUnlockStatus();
            if (!current.isUnlocked || current.activeUserId !== initial.activeUserId ||
                current.unlockedUserId !== initial.unlockedUserId) throw new GuiError('lease_interrupted');
        });
        const profiles = await scope.step(() => loadSavedServerProfilesStrict());
        const matches = profiles.filter(profile => profile.id === profileId);
        if (matches.length !== 1) throw new GuiError('invalid_args');
        return connect(matches[0], scope);
    };
}
