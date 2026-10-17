// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { TranslationFunction } from '../../i18n/types';

/** What `vault_v3_repair` and `aerovz_repair` return. */
export interface RepairResult {
    repaired?: number;
    damaged?: number;
    dry_run?: boolean;
}

/**
 * The repair dialog's summary of a repair or a dry run.
 *
 * The engine is all-or-nothing: a non-zero `repaired` with damaged data blocks
 * means every one verified (and, unless dry run, was persisted). `repaired` is
 * also 1 with no damaged data block when opening the vault had to rebuild its
 * header or manifest from parity: the repair re-seals the vault with it, so that
 * case is not "no damage". A dry run writes nothing and says so.
 */
export function repairResultMessage(result: RepairResult, t: TranslationFunction): string {
    const repaired = result.repaired ?? 0;
    const damaged = result.damaged ?? 0;
    const dry = !!result.dry_run;
    if (damaged === 0 && repaired > 0) return dry ? t('vault.repairDryRunMetadata') : t('vault.metadataRepaired');
    if (damaged === 0) return dry ? t('vault.repairDryRunNoDamage') : t('vault.noDamageNothing');
    if (repaired === 0) {
        return dry
            ? t('vault.repairDryRunCannotRepair', { damaged: String(damaged) })
            : t('vault.couldNotRepairAny', { damaged: String(damaged) });
    }
    return dry
        ? t('vault.repairDryRunCanRepair', { repaired: String(repaired) })
        : t('vault.successfullyRepaired', { repaired: String(repaired) });
}
