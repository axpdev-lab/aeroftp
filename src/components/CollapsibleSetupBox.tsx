// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Collapsible "Setup … first" instruction box for Quick Connect (#215 idea D).
 *
 * The setup-steps boxes for the local-bridge modes (Filen Desktop / MEGAcmd)
 * are tall. This wraps them in a click-to-collapse panel with a chevron. When a
 * `bridgeState` is supplied it auto-collapses while the bridge is active (🟢)
 * and auto-expands otherwise (🔴/🟠) — until the user manually toggles, after
 * which the choice sticks until the bridge state actually changes again. With no
 * `bridgeState` (non-bridge providers) it is simply a collapsible defaulting to
 * open.
 */
import React, { useEffect, useRef, useState } from 'react';
import { ChevronDown } from 'lucide-react';
import type { BridgeUiState } from '../hooks/useBridgeStatus';

type Tone = 'amber' | 'red';

const TONE: Record<Tone, string> = {
    amber:
        'border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-500/30 dark:bg-amber-500/10 dark:text-amber-100',
    red: 'border-red-100 bg-red-50 text-red-900 dark:border-red-900/30 dark:bg-red-900/10 dark:text-red-100',
};

/**
 * Default open/closed state for the setup box (pure, for tests). A non-bridge box
 * is always open. A bridge box is collapsed while loading (`bridgeState`
 * undefined) and on 🟢, and open only once it settles to a non-green state — so it
 * never flashes open during "Checking…" and then collapses on 🟢 (#215 idea D).
 */
export function setupBoxDefaultOpen(
    isBridge: boolean | undefined,
    bridgeState: BridgeUiState | undefined,
): boolean {
    if (!isBridge) return true;
    if (bridgeState === undefined) return false;
    return bridgeState !== 'green';
}

interface CollapsibleSetupBoxProps {
    title: React.ReactNode;
    tone?: Tone;
    /** SETTLED bridge state (undefined while the probe is still loading, or for
     *  non-bridge providers). Drives auto-collapse on 🟢 / auto-expand otherwise. */
    bridgeState?: BridgeUiState;
    /** True when this box belongs to a local-bridge mode. A bridge box starts
     *  COLLAPSED (the common case is an active bridge -> 🟢 -> collapsed) and only
     *  expands once the state settles to a non-green value, so it never flashes
     *  open during "Checking…" and then collapses on 🟢 (#215 idea D). */
    isBridge?: boolean;
    children: React.ReactNode;
}

export const CollapsibleSetupBox: React.FC<CollapsibleSetupBoxProps> = ({
    title,
    tone = 'amber',
    bridgeState,
    isBridge,
    children,
}) => {
    const [open, setOpen] = useState<boolean>(setupBoxDefaultOpen(isBridge, bridgeState));
    const prevState = useRef<BridgeUiState | undefined>(bridgeState);

    // Re-apply the default open/closed state on a real state transition (🟢 ->
    // collapse, 🔴/🟠 -> expand, loading stays collapsed for a bridge box). A
    // manual toggle in between is respected because we only act on an actual
    // change of bridgeState.
    useEffect(() => {
        if (bridgeState !== prevState.current) {
            prevState.current = bridgeState;
            setOpen(setupBoxDefaultOpen(isBridge, bridgeState));
        }
    }, [bridgeState, isBridge]);

    return (
        <div className={`rounded-lg border p-3 text-xs ${TONE[tone]}`}>
            <button
                type="button"
                onClick={() => setOpen(o => !o)}
                aria-expanded={open}
                className="flex w-full items-center justify-between gap-2 font-medium text-left"
            >
                <span>{title}</span>
                <ChevronDown
                    size={14}
                    className={`shrink-0 transition-transform ${open ? '' : '-rotate-90'}`}
                />
            </button>
            {open && <div className="mt-2">{children}</div>}
        </div>
    );
};

export default CollapsibleSetupBox;
