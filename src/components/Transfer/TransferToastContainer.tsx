/**
 * TransferToastContainer — Isolated toast state management
 *
 * Subscribes to 'transfer-toast-update' custom DOM events to update the toast
 * WITHOUT causing the parent (App.tsx) to re-render. This prevents the entire
 * file browser from re-rendering on every progress tick, which caused visible
 * theme flicker in WebKitGTK.
 */

import React, { useState, useEffect, useRef, useCallback } from 'react';
import { TransferToast } from './index';
import { TransferProgress } from '../../types';

interface TransferToastContainerProps {
    onCancel: () => void;
}

/** Custom event name for transfer progress updates */
export const TRANSFER_TOAST_EVENT = 'transfer-toast-update';

/** Dispatch a transfer toast update (called from useTransferEvents) */
export function dispatchTransferToast(transfer: TransferProgress | null): void {
    window.dispatchEvent(new CustomEvent(TRANSFER_TOAST_EVENT, { detail: transfer }));
}

export const TransferToastContainer: React.FC<TransferToastContainerProps> = ({ onCancel }) => {
    const [transfer, setTransfer] = useState<TransferProgress | null>(null);
    const lastProgressUpdate = useRef<number>(Date.now());

    // Subscribe to transfer toast events
    useEffect(() => {
        const handler = (e: Event) => {
            const detail = (e as CustomEvent<TransferProgress | null>).detail;
            setTransfer(detail);
            if (detail) lastProgressUpdate.current = Date.now();
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
    }, [transfer?.percentage]);

    const handleCancel = useCallback(() => {
        setTransfer(null);
        onCancel();
    }, [onCancel]);

    if (!transfer) return null;
    return <TransferToast transfer={transfer} onCancel={handleCancel} />;
};
