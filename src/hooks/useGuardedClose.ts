// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useGuardedClose: protects a modal from being dismissed while it would lose
 * work. Two guard kinds:
 *   - 'busy'  an operation is running (e.g. AeroSync, CrossProfile transfer):
 *             closing should offer to wait or stop the operation.
 *   - 'dirty' there are unsaved edits (e.g. AeroImage edit mode): closing
 *             should offer to keep editing or discard.
 *
 * Contract:
 *   - the backdrop (click-outside) is inert while guarded, so a stray click
 *     can never destroy work;
 *   - the X button and ESC route through `requestClose`, which opens an inline
 *     confirm when guarded and closes immediately otherwise.
 *
 * Pair with <GuardedCloseConfirm> for the confirm UI. New operation/edit modals
 * inherit the same behaviour by passing their own `guard` signal.
 */

import { useCallback, useState } from 'react';

export type GuardKind = 'busy' | 'dirty';

interface UseGuardedCloseOptions {
  /** Active guard, or null/false/undefined when the modal is free to close. */
  guard: GuardKind | null | false | undefined;
  /** Perform the actual close. */
  onClose: () => void;
  /** For a 'busy' guard: abort the running operation, invoked right before the
   *  close when the user confirms "stop and close". */
  onAbort?: () => void;
}

export interface GuardedClose {
  /** True while the confirm overlay should be shown. */
  confirmOpen: boolean;
  /** Which guard triggered the confirm (drives the confirm copy). */
  confirmKind: GuardKind | null;
  /** Wire to the X button and ESC: asks when guarded, closes otherwise. */
  requestClose: () => void;
  /** Wire to the backdrop onClick: inert while guarded, closes otherwise. */
  requestBackdropClose: () => void;
  /** Confirm action: abort the operation (if 'busy') then close. */
  confirmAndClose: () => void;
  /** Dismiss the confirm and keep working. */
  cancelConfirm: () => void;
}

export function useGuardedClose({ guard, onClose, onAbort }: UseGuardedCloseOptions): GuardedClose {
  const [confirmKind, setConfirmKind] = useState<GuardKind | null>(null);

  const requestClose = useCallback(() => {
    if (guard) setConfirmKind(guard);
    else onClose();
  }, [guard, onClose]);

  const requestBackdropClose = useCallback(() => {
    if (!guard) onClose();
    // guarded -> inert: a click outside never closes a working modal.
  }, [guard, onClose]);

  const confirmAndClose = useCallback(() => {
    if (confirmKind === 'busy') onAbort?.();
    setConfirmKind(null);
    onClose();
  }, [confirmKind, onAbort, onClose]);

  const cancelConfirm = useCallback(() => setConfirmKind(null), []);

  return {
    confirmOpen: confirmKind !== null,
    confirmKind,
    requestClose,
    requestBackdropClose,
    confirmAndClose,
    cancelConfirm,
  };
}
