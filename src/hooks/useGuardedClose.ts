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
 *     can never destroy work; with `backdrop: 'never'` it is inert always: a
 *     modal holding a costly result (a comparison, a plan, test results, an
 *     opened vault) or a prompt that asks for a decision is closed only by
 *     its X, its buttons or ESC;
 *   - the X button and ESC route through `requestClose`, which opens an inline
 *     confirm when guarded and closes immediately otherwise; `escape` makes
 *     the hook route ESC itself, while the modal is open.
 *
 * Pair with <GuardedCloseConfirm> for the confirm UI. New operation/edit modals
 * inherit the same behaviour by passing their own `guard` signal.
 */

import type React from 'react';
import { useCallback, useEffect, useRef, useState } from 'react';

export type GuardKind = 'busy' | 'dirty';

/** When a click on the backdrop may close: 'idle' while no guard is set, as
 *  a plain modal does; 'never' for a modal whose result or question a stray
 *  click must not throw away. */
export type BackdropPolicy = 'idle' | 'never';

interface UseGuardedCloseOptions {
  /** Active guard, or null/false/undefined when the modal is free to close. */
  guard: GuardKind | null | false | undefined;
  /** Perform the actual close. */
  onClose: () => void;
  /** For a 'busy' guard: abort the running operation, invoked right before the
   *  close when the user confirms "stop and close". */
  onAbort?: () => void;
  /** When the backdrop may close (default 'idle'). */
  backdrop?: BackdropPolicy;
  /** Route ESC to `requestClose` while true (the modal is open). Leave it
   *  unset for a modal that handles ESC itself. */
  escape?: boolean;
  /** While true the modal keeps its place among the modals that route ESC,
   *  but answers no press: something of its own open over it (a dialog that
   *  handles ESC itself, a request that must finish) owns the key. Switching
   *  `escape` off and on instead would move the modal to the top. */
  escapeSuspended?: boolean;
}

export interface GuardedClose {
  /** True while the confirm overlay should be shown. */
  confirmOpen: boolean;
  /** Which guard triggered the confirm (drives the confirm copy). */
  confirmKind: GuardKind | null;
  /** Wire to the X button and ESC: asks when guarded, closes otherwise. */
  requestClose: () => void;
  /** Wire to the backdrop onClick: inert while guarded or with
   *  `backdrop: 'never'`, closes otherwise. */
  requestBackdropClose: () => void;
  /** The same for a backdrop that wraps the dialog: only a click on the
   *  backdrop itself counts, never one inside the dialog. */
  onBackdropClick: (e: React.MouseEvent) => void;
  /** Whether "stop" aborts something (an `onAbort` was given): without one the
   *  operation goes on after the close, and the confirm says so. */
  canAbort: boolean;
  /** Confirm action: abort the operation (if 'busy') then close. */
  confirmAndClose: () => void;
  /** Dismiss the confirm and keep working. */
  cancelConfirm: () => void;
}

// The modals that route ESC through this hook, in the order they opened. Only
// the last one answers a press: window listeners run in registration order,
// so without this a panel opened first (AeroSync) would take the ESC meant
// for a prompt opened over it.
const escapeStack: symbol[] = [];

/** Whether an open modal routes ESC through this hook. A handler that closes
 *  something underneath (the app-wide ESC shortcut, the Settings panel) must
 *  leave the key alone then: it would otherwise take or swallow the ESC meant
 *  for that modal. */
export function isEscapeClaimed(): boolean {
  return escapeStack.length > 0;
}

export function useGuardedClose({ guard, onClose, onAbort, backdrop = 'idle', escape = false, escapeSuspended = false }: UseGuardedCloseOptions): GuardedClose {
  const [confirmKind, setConfirmKind] = useState<GuardKind | null>(null);
  const tokenRef = useRef<symbol | null>(null);

  // A confirm asks about an operation or input that is there now: once the
  // guard is gone (the operation ended, the input was saved) it has nothing
  // left to ask, and a modal that stays mounted must not show it next time.
  // While it is open it follows the guard, so it always says what a close
  // would lose now (an operation that finished leaves typed input behind).
  useEffect(() => {
    setConfirmKind(current => (current === null ? null : guard || null));
  }, [guard]);

  const requestClose = useCallback(() => {
    if (guard) setConfirmKind(guard);
    else onClose();
  }, [guard, onClose]);

  const requestBackdropClose = useCallback(() => {
    // Guarded, or a result or question to keep: a click outside never closes.
    if (backdrop === 'never' || guard) return;
    onClose();
  }, [backdrop, guard, onClose]);

  const onBackdropClick = useCallback((e: React.MouseEvent) => {
    if (e.target === e.currentTarget) requestBackdropClose();
  }, [requestBackdropClose]);

  // ESC asks like the X does. The confirm takes ESC first (capture phase), so
  // an open confirm is answered "keep" and the modal stays.
  // The place in the stack is held for as long as ESC is enabled, across the
  // confirm, so a modal keeps its rank while it asks.
  useEffect(() => {
    if (!escape) return;
    const token = Symbol('guarded-close');
    escapeStack.push(token);
    tokenRef.current = token;
    return () => {
      escapeStack.splice(escapeStack.indexOf(token), 1);
      tokenRef.current = null;
    };
  }, [escape]);
  useEffect(() => {
    if (!escape || escapeSuspended || confirmKind) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key !== 'Escape' || e.defaultPrevented) return;
      if (escapeStack[escapeStack.length - 1] !== tokenRef.current) return;
      e.preventDefault();
      requestClose();
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [escape, escapeSuspended, confirmKind, requestClose]);

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
    onBackdropClick,
    canAbort: !!onAbort,
    confirmAndClose,
    cancelConfirm,
  };
}
