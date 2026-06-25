// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * VaultCreateReceipt: the terminal step of an AeroVault/AeroVault Zip
 * creation, mirroring the Compressor flow (CompressDialog's CompletionStats).
 * Creating no longer drops the user into the file browser: instead they get a
 * green-check confirmation with before/after bars and a "Done" button that
 * closes the modal. To inspect the produced vault they double-click it later,
 * which opens the browser (VaultPanel `open` mode) as before.
 *
 * The detailed behind-the-scenes technical receipt (VaultReceipt, Ehud #162)
 * stays available on demand via the "Technical Receipt" button so nothing is
 * lost; it just no longer auto-overlays on top of a freshly opened browser.
 */

import * as React from 'react';
import { useState } from 'react';
import { Check, Package, Lock, Shrink, ShieldPlus, TrendingUp } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { VaultState } from './useVaultState';
import { ZipCompressionReport } from './ZipCompressionReport';
import { VaultReceipt } from './VaultReceipt';
// Same Compressor --compress-* theme as the create surface, so create + receipt
// read as one Compressor-style flow (the owner's redesign, Phase 2).
import '../CompressDialog.css';

interface VaultCreateReceiptProps {
    state: VaultState;
    onClose: () => void;
}

export const VaultCreateReceipt: React.FC<VaultCreateReceiptProps> = ({ state, onClose }) => {
    const t = useTranslation();
    const [showTechnical, setShowTechnical] = useState(false);

    // Before/after bars, reusing the Compressor-styled ZipCompressionReport.
    // Plaintext .aerozip carries an explicit {input,output} report; encrypted
    // vaults derive it from the technical report (plaintext in vs encrypted
    // container out). Empty / v1 vaults have neither, so the bars are omitted
    // (an honest "no data" rather than a fabricated 0%).
    const bars = state.zipReport
        ? state.zipReport
        : (state.lastReport && state.lastReport.plaintext_bytes > 0)
            ? { inputBytes: state.lastReport.plaintext_bytes, outputBytes: state.lastReport.encrypted_bytes }
            : null;

    const fileName = state.vaultPath.split(/[\\/]/).pop() || '';
    const fileCount = state.meta?.fileCount ?? 0;
    const heading = state.isPlaintextZip ? t('vault.zipCreated') : t('vault.created');

    // Size-composition badges: explain what the final container weight is made of,
    // so an "increase" (encryption + container overhead) or a "saving"
    // (compression) is self-evident instead of surprising. The factors are
    // derived from the create parameters, in order, and colour-coded by role:
    // container/encryption are neutral structural overhead (slate), compression is
    // the only size-reducing factor (emerald), and Error Correction adds weight
    // back (amber, with an up-arrow). Compression participates only for the v3
    // Archive level and plaintext .aerozip; Error Correction only where it was
    // offered (those same two modes) and actually enabled.
    const hasEncryption = !state.isPlaintextZip;
    const hasCompression = state.isPlaintextZip || state.securityLevel === 'experimental';
    const hasRecovery = state.errorCorrectionEnabled && (state.isPlaintextZip || state.securityLevel === 'experimental');
    const toneClass: Record<'slate' | 'emerald' | 'amber', string> = {
        slate: 'bg-slate-500/10 text-slate-500 dark:text-slate-300',
        emerald: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400',
        amber: 'bg-amber-500/10 text-amber-600 dark:text-amber-400',
    };
    const factors: { key: string; label: string; icon: React.ReactNode; tone: 'slate' | 'emerald' | 'amber' }[] = [
        { key: 'container', label: t('vault.receipt.factorContainer'), icon: <Package size={11} />, tone: 'slate' },
        ...(hasEncryption ? [{ key: 'encryption', label: t('protocol.encryption'), icon: <Lock size={11} />, tone: 'slate' as const }] : []),
        ...(hasCompression ? [{ key: 'compression', label: t('syncPanel.compression'), icon: <Shrink size={11} />, tone: 'emerald' as const }] : []),
        ...(hasRecovery ? [{ key: 'recovery', label: t('aerosync.errorCorrection'), icon: <ShieldPlus size={11} />, tone: 'amber' as const }] : []),
    ];

    return (
        <div className="compress-dialog flex flex-col" style={{ color: 'var(--compress-text)' }}>
            <div className="px-6 pt-6 pb-2 flex flex-col items-center text-center">
                <span className="flex items-center justify-center w-12 h-12 rounded-full bg-emerald-500/15 mb-3">
                    <Check size={24} className="text-emerald-500" />
                </span>
                <h3 className="text-base font-semibold" style={{ color: 'var(--compress-text)' }}>{heading}</h3>
                {fileName && (
                    <div className="mt-1 max-w-full truncate font-mono text-xs" style={{ color: 'var(--compress-text-secondary)' }} title={state.vaultPath}>
                        {fileName}
                    </div>
                )}
                {fileCount > 0 && (
                    <div className="mt-0.5 text-[11px]" style={{ color: 'var(--compress-text-muted)' }}>
                        {fileCount} {t('vault.receipt.files')}
                    </div>
                )}
            </div>

            {bars && <ZipCompressionReport inputBytes={bars.inputBytes} outputBytes={bars.outputBytes} />}

            {/* Size-composition badges: which factors make up the final weight,
                in order, colour-coded (slate = structural overhead, emerald =
                compression/the only reducer, amber = Error Correction/adds weight). */}
            {bars && factors.length > 0 && (
                <div className="px-4 pb-3">
                    <div className="text-[10px] mb-1.5 text-center" style={{ color: 'var(--compress-text-muted)' }}>
                        {t('vault.receipt.composition')}
                    </div>
                    <div className="flex items-center justify-center gap-1.5 flex-wrap">
                        {factors.map((f, i) => (
                            <React.Fragment key={f.key}>
                                {i > 0 && <span className="text-[11px]" style={{ color: 'var(--compress-text-muted)' }}>+</span>}
                                <span className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium ${toneClass[f.tone]}`}>
                                    {f.icon}{f.label}{f.tone === 'amber' && <TrendingUp size={10} />}
                                </span>
                            </React.Fragment>
                        ))}
                    </div>
                </div>
            )}

            <div className="flex items-center justify-end gap-2 px-6 py-4 border-t" style={{ borderColor: 'var(--compress-border)' }}>
                {state.lastReport && (
                    <button
                        onClick={() => setShowTechnical(true)}
                        className="px-4 py-2 rounded-lg text-sm font-medium transition-colors"
                        style={{ background: 'var(--compress-bg-hover)', color: 'var(--compress-text-secondary)' }}
                    >
                        {t('vault.receipt.title')}
                    </button>
                )}
                <button
                    onClick={onClose}
                    className="flex items-center gap-2 px-5 py-2 rounded-lg text-sm font-medium text-white transition-colors"
                    style={{ background: 'var(--compress-accent)' }}
                    onMouseEnter={e => (e.currentTarget.style.background = 'var(--compress-accent-hover)')}
                    onMouseLeave={e => (e.currentTarget.style.background = 'var(--compress-accent)')}
                >
                    <Check size={15} />
                    {t('compress.done')}
                </button>
            </div>

            {/* On-demand technical receipt; closing it keeps the report so the
                bars and the button stay intact (do not call clearReport here). */}
            {showTechnical && state.lastReport && (
                <VaultReceipt report={state.lastReport} t={t} onClose={() => setShowTechnical(false)} />
            )}
        </div>
    );
};
