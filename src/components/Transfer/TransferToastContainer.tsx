// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * TransferToastContainer: Isolated toast state management
 *
 * Subscribes to 'transfer-toast-update' custom DOM events to update the toast
 * WITHOUT causing the parent (App.tsx) to re-render. This prevents the entire
 * file browser from re-rendering on every progress tick, which caused visible
 * theme flicker in WebKitGTK.
 */

import React, { useState, useEffect, useRef, useCallback } from 'react';
import { TransferToast, TransferToastState } from './index';

/** Custom event name for transfer progress updates */
export const TRANSFER_TOAST_EVENT = 'transfer-toast-update';
let latestTransferToastState: TransferToastState | null = null;
let activeTransferToastId: string | null = null;
let dismissedTransferToastId: string | null = null;

/** Dispatch a transfer toast update (called from useTransferEvents) */
export function dispatchTransferToast(transfer: TransferToastState | null): void {
    if (!transfer) {
        latestTransferToastState = null;
        activeTransferToastId = null;
        dismissedTransferToastId = null;
    } else {
        latestTransferToastState = transfer;
        const nextTransferId = transfer.summary.transfer_id || null;
        if (nextTransferId && nextTransferId !== activeTransferToastId) {
            dismissedTransferToastId = null;
        }
        activeTransferToastId = nextTransferId;
        if (nextTransferId && nextTransferId === dismissedTransferToastId) {
            return;
        }
    }
    window.dispatchEvent(new CustomEvent(TRANSFER_TOAST_EVENT, { detail: transfer }));
}

export function dismissTransferToast(): void {
    dismissedTransferToastId = activeTransferToastId;
    window.dispatchEvent(new CustomEvent(TRANSFER_TOAST_EVENT, { detail: null }));
}

export function reopenTransferToast(): void {
    if (!latestTransferToastState) return;
    dismissedTransferToastId = null;
    window.dispatchEvent(new CustomEvent(TRANSFER_TOAST_EVENT, { detail: latestTransferToastState }));
}

export const TransferToastContainer: React.FC = () => {
    const [transfer, setTransfer] = useState<TransferToastState | null>(null);
    const lastProgressUpdate = useRef<number>(Date.now());

    // Subscribe to transfer toast events
    useEffect(() => {
        const handler = (e: Event) => {
            const detail = (e as CustomEvent<TransferToastState | null>).detail;
            if (!detail) {
                setTransfer(null);
                return;
            }
            if (detail.summary.transfer_id && detail.summary.transfer_id === dismissedTransferToastId) {
                return;
            }
            setTransfer(detail);
            lastProgressUpdate.current = Date.now();
        };
        window.addEventListener(TRANSFER_TOAST_EVENT, handler);
        return () => window.removeEventListener(TRANSFER_TOAST_EVENT, handler);
    }, []);

    // Stuck detection: auto-close if no updates for 30 seconds
    useEffect(() => {
        if (!transfer) return;

        lastProgressUpdate.current = Date.now();

        const checkStuck = setInterval(() => {
            if (Date.now() - lastProgressUpdate.current > 30000) {
                setTransfer(null);
                // Also dispatch so App.tsx hasActiveTransfer stays in sync
                dispatchTransferToast(null);
            }
        }, 5000);

        return () => clearInterval(checkStuck);
    }, [transfer?.summary.percentage]);

    const handleCancel = useCallback(() => {
        setTransfer(null);
        dismissTransferToast();
    }, []);

    if (!transfer) return null;
    return <TransferToast transfer={transfer} onCancel={handleCancel} />;
};
