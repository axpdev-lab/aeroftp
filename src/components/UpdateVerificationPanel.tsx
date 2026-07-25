// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Verification result for a downloaded update.
 *
 * Shows what the app can actually prove about the artifact: that it was verified
 * against the Sigstore transparency log, by which CI identity, under which Rekor
 * entry, and whether the digest on disk matches the one that was signed. When
 * there is nothing to prove it says so plainly instead of borrowing the language
 * of verification.
 *
 * Two variants of the same content: `panel` for the update toast, `inline` for
 * the fullscreen install overlay, where only the headline fits.
 */

import React, { useState } from 'react';
import { ShieldCheck, ShieldQuestion, ShieldAlert, ChevronDown, ExternalLink, Check, X, type LucideIcon } from 'lucide-react';
import { SigstoreMark } from './icons/SigstoreMark';
import { openUrl } from '../utils/openUrl';
import {
    classifyUpdateVerification,
    formatIntegratedTime,
    rekorSearchUrl,
    shortenDigest,
    type UpdateVerificationInfo,
    type UpdateVerifyTone,
} from '../utils/updateVerification';

interface UpdateVerificationPanelProps {
    verification: UpdateVerificationInfo;
    t: (key: string, params?: Record<string, string | number>) => string;
    /** `panel` = full card (toast); `inline` = single headline row (overlay). */
    variant?: 'panel' | 'inline';
    locale?: string;
}

const TONE_BOX: Record<UpdateVerifyTone, string> = {
    verified: 'bg-green-500/10 border-green-500/20 text-green-300',
    'sha-only': 'bg-slate-400/10 border-slate-400/25 text-slate-200',
    unverified: 'bg-amber-500/10 border-amber-500/25 text-amber-300',
    failed: 'bg-red-500/10 border-red-500/20 text-red-300',
};

const TONE_TEXT: Record<UpdateVerifyTone, string> = {
    verified: 'text-green-400',
    'sha-only': 'text-slate-300',
    unverified: 'text-amber-400',
    failed: 'text-red-400',
};

const TONE_ICON: Record<UpdateVerifyTone, LucideIcon> = {
    verified: ShieldCheck,
    'sha-only': ShieldQuestion,
    unverified: ShieldQuestion,
    failed: ShieldAlert,
};

/** Headline for each tone. Never claims a check that did not happen. */
function headline(tone: UpdateVerifyTone, t: UpdateVerificationPanelProps['t']): string {
    switch (tone) {
        case 'verified': return t('update.verifiedWithSigstore');
        case 'sha-only': return t('update.verifyNoSignature');
        case 'unverified': return t('update.verifyCouldNotVerify');
        case 'failed': return t('update.verifySignatureFailed');
    }
}

/** One label/value line of the evidence list. */
const Row: React.FC<{ label: string; children: React.ReactNode; title?: string }> = ({ label, children, title }) => (
    <div className="grid grid-cols-[62px_1fr] gap-2 items-baseline font-mono text-[11px]">
        <span className="opacity-60 uppercase tracking-wide text-[9.5px]">{label}</span>
        <span className="truncate" title={title}>{children}</span>
    </div>
);

export const UpdateVerificationPanel: React.FC<UpdateVerificationPanelProps> = ({
    verification: v,
    t,
    variant = 'panel',
    locale,
}) => {
    const [showDetails, setShowDetails] = useState(false);
    const tone = classifyUpdateVerification(v);
    const VIcon = TONE_ICON[tone];
    const label = headline(tone, t);

    if (variant === 'inline') {
        return (
            <div className={`flex items-center gap-1.5 text-xs mt-2 ${TONE_TEXT[tone]}`}>
                <VIcon size={13} className="flex-shrink-0" />
                <span>{label}</span>
                {tone === 'verified' && <SigstoreMark size={13} className="rounded-[3px] bg-white/90 p-[1px]" />}
            </div>
        );
    }

    const integratedOn = formatIntegratedTime(v.rekor_integrated_time, locale);
    const signer = v.workflow_identity
        // ".../<owner>/<repo>/.github/workflows/<file>@<ref>" -> "<owner>/<repo> · <file>"
        ? (() => {
            const match = v.workflow_identity.match(/github\.com\/([^/]+\/[^/]+)\/\.github\/workflows\/([^@]+)/);
            return match ? `${match[1]} · ${match[2]}` : v.workflow_identity;
        })()
        : null;

    return (
        <div className={`my-1.5 flex flex-col gap-1.5 text-xs border rounded-lg p-2 ${TONE_BOX[tone]}`}>
            <div className="flex items-center justify-between gap-2 font-medium">
                <div className="flex items-center gap-1.5 truncate">
                    <VIcon size={14} className="flex-shrink-0" />
                    <span className="truncate">{label}</span>
                </div>
                {v.bundle_present && (
                    <div className="flex items-center gap-1.5 flex-shrink-0">
                        <SigstoreMark size={17} className="rounded-[5px] bg-white/95 px-[3px] py-[2px]" />
                        {v.bundle_version && (
                            <span className="font-mono text-[9.5px] tracking-wide opacity-75">v{v.bundle_version}</span>
                        )}
                    </div>
                )}
            </div>

            {/* Tailwind 3 cannot apply an alpha modifier to `currentColor`, so the
                separator uses a fixed translucent white on these tinted grounds. */}
            <dl className="flex flex-col gap-0.5 border-t border-white/15 pt-1.5 opacity-95 m-0">
                {signer && <Row label={t('update.verifySigner')} title={v.workflow_identity ?? undefined}>{signer}</Row>}

                {v.rekor_log_index !== null && (
                    <Row label={t('update.verifyRekor')}>
                        {/* Opens the public Rekor entry in the system browser: anyone
                            can re-check this release without trusting the app. */}
                        <button
                            type="button"
                            onClick={() => { void openUrl(rekorSearchUrl(v.rekor_log_index as number)); }}
                            className="inline-flex items-center gap-1 border-b border-dotted border-current hover:opacity-80 transition-opacity"
                            title={t('update.verifyRekorOpen')}
                        >
                            #{v.rekor_log_index}
                            <ExternalLink size={9} className="flex-shrink-0" />
                        </button>
                        {integratedOn && <span className="opacity-70"> · {integratedOn}</span>}
                    </Row>
                )}

                <Row label={t('update.verifyDigest')} title={v.artifact_sha256}>
                    {shortenDigest(v.artifact_sha256)}
                    {v.digest_match === true && (
                        <span className="text-green-300 ml-1 whitespace-nowrap">
                            <Check size={10} className="inline align-[-1px]" /> {t('update.verifyHashMatch')}
                        </span>
                    )}
                    {v.digest_match === false && (
                        <span className="text-red-300 ml-1 whitespace-nowrap">
                            <X size={10} className="inline align-[-1px]" /> {t('update.verifyDigestDiffers')}
                        </span>
                    )}
                </Row>

                {/* Only worth showing when it is evidence of a mismatch: on a match
                    it would just repeat the line above. */}
                {v.digest_match === false && v.attested_sha256 && (
                    <Row label={t('update.verifySignedDigest')} title={v.attested_sha256}>
                        {shortenDigest(v.attested_sha256)}
                    </Row>
                )}
            </dl>

            {tone === 'sha-only' && (
                <p className="text-[11px] opacity-80 leading-snug m-0" style={{ textWrap: 'pretty' } as React.CSSProperties}>
                    {t('update.verifyComputedLocally')}
                </p>
            )}
            {tone === 'unverified' && (
                <p className="text-[11px] opacity-80 leading-snug m-0" style={{ textWrap: 'pretty' } as React.CSSProperties}>
                    {t('update.verifyInstallStillAllowed')}
                </p>
            )}

            {v.bundle_present && (
                <>
                    <button
                        type="button"
                        onClick={() => setShowDetails(open => !open)}
                        aria-expanded={showDetails}
                        className="self-start flex items-center gap-1 text-[11px] opacity-70 hover:opacity-100 transition-opacity"
                    >
                        <ChevronDown size={11} className={`transition-transform ${showDetails ? 'rotate-180' : ''}`} />
                        {t('update.verifyDetails')}
                    </button>
                    {showDetails && (
                        <dl className="flex flex-col gap-0.5 opacity-95 m-0">
                            {v.workflow_identity && (
                                <Row label={t('update.verifyIdentity')} title={v.workflow_identity}>
                                    {v.workflow_identity.replace(/^https:\/\/github\.com\/[^/]+\/[^/]+\//, '…/')}
                                </Row>
                            )}
                            {v.oidc_issuer && (
                                <Row label={t('update.verifyIssuer')} title={v.oidc_issuer}>
                                    {v.oidc_issuer.replace(/^https:\/\//, '')}
                                </Row>
                            )}
                            {v.rekor_entry_kind && <Row label={t('update.verifyEntry')}>{v.rekor_entry_kind}</Row>}
                            <Row label={t('update.verifyVerifier')}>sigstore-rs {v.verifier_version}</Row>
                        </dl>
                    )}
                </>
            )}
        </div>
    );
};

export default UpdateVerificationPanel;
