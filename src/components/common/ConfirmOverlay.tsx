// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlertTriangle } from 'lucide-react';
import { useTranslation } from '../../i18n';

/**
 * The one styled confirmation for a component that raises its own.
 *
 * `window.confirm` is not an option in this app and has not been since the
 * v2.6.4 audit closed H18: WebKitGTK does not implement it, so on Linux it
 * returns without ever drawing anything and the destructive branch runs as if
 * the user had said yes. That is how deleting a saved server used to happen
 * silently, and six call sites had drifted back to it since, one of them the
 * account delete in Manage Users.
 *
 * `zClass` must be a literal from `MODAL_Z`: Tailwind builds its utilities by
 * scanning the source, so a computed class name never reaches the stylesheet
 * and the element falls back to `z-index: auto`. Render this *inside* the
 * overlay of the component that raises it, so it lives in that component's own
 * stacking context and cannot be painted over by the modal that is waiting on
 * the answer, which is #537.
 */
interface ConfirmOverlayProps {
    /** The question. `\n` is preserved, so a two-paragraph prompt renders as one. */
    message: string;
    onConfirm: () => void;
    onCancel: () => void;
    /** Defaults to `common.delete`. */
    confirmLabel?: string;
    confirmColor?: 'red' | 'blue';
    /** A literal from `MODAL_Z`, matching the tier of the overlay this sits in. */
    zClass: string;
}

export const ConfirmOverlay: React.FC<ConfirmOverlayProps> = ({
    message,
    onConfirm,
    onCancel,
    confirmLabel,
    confirmColor = 'red',
    zClass,
}) => {
    const t = useTranslation();
    const cancelRef = React.useRef<HTMLButtonElement>(null);

    // Read through a ref so this effect runs once per mount. Every call site
    // passes an inline arrow for `onCancel`, so depending on it would tear the
    // listener down and re-run on each caller re-render, and `focus()` would
    // pull focus back to Cancel each time. That is reachable: CloudPanel
    // re-renders on Tauri cloud events, which can fire while the question is up.
    const onCancelRef = React.useRef(onCancel);
    onCancelRef.current = onCancel;

    const boxRef = React.useRef<HTMLDivElement>(null);

    // Escape answers "no". A confirmation with no keyboard way out is the same
    // trap as one that cannot be clicked.
    //
    // Tab is held inside the two buttons, and focus goes back where it came from
    // on close. Without the trap, tabbing walks straight out of a dialog that
    // declares `aria-modal` into the panel underneath, which is still there:
    // these overlays are rendered inside the modal that raised them and hide
    // nothing. Without the restore, answering a question moves focus to the body
    // and a keyboard user starts the next tab run from the top of the page.
    React.useEffect(() => {
        const returnTo = document.activeElement as HTMLElement | null;
        const onKey = (event: KeyboardEvent) => {
            if (event.key === 'Escape') {
                event.stopPropagation();
                onCancelRef.current();
                return;
            }
            if (event.key !== 'Tab') return;
            const focusable = boxRef.current?.querySelectorAll<HTMLElement>('button:not([disabled])');
            if (!focusable || focusable.length === 0) return;
            const first = focusable[0];
            const last = focusable[focusable.length - 1];
            const active = document.activeElement;
            if (event.shiftKey && (active === first || !boxRef.current?.contains(active))) {
                event.preventDefault();
                last.focus();
            } else if (!event.shiftKey && (active === last || !boxRef.current?.contains(active))) {
                event.preventDefault();
                first.focus();
            }
        };
        document.addEventListener('keydown', onKey, true);
        cancelRef.current?.focus();
        return () => {
            document.removeEventListener('keydown', onKey, true);
            returnTo?.focus?.();
        };
    }, []);

    const confirmColorClass =
        confirmColor === 'red' ? 'bg-red-500 hover:bg-red-600' : 'bg-blue-500 hover:bg-blue-600';

    return (
        <div
            className={`fixed inset-0 ${zClass} flex items-center justify-center bg-black/50`}
            role="dialog"
            aria-modal="true"
            aria-label={message}
            // Several of the overlays this renders inside close themselves on a
            // backdrop click. Without stopping here, dismissing the question
            // would also dismiss the panel that asked it.
            onClick={(event) => {
                event.stopPropagation();
                onCancel();
            }}
        >
            <div
                ref={boxRef}
                className="mx-4 max-w-sm animate-scale-in rounded-lg border border-gray-200 bg-white p-6 shadow-2xl dark:border-gray-700 dark:bg-gray-800"
                onClick={(event) => event.stopPropagation()}
            >
                <div className="mb-4 flex items-start gap-3">
                    <AlertTriangle size={20} className="mt-0.5 shrink-0 text-amber-500" />
                    <p className="whitespace-pre-line text-sm text-gray-900 dark:text-gray-100">{message}</p>
                </div>
                <div className="flex justify-end gap-2">
                    <button
                        ref={cancelRef}
                        type="button"
                        onClick={onCancel}
                        className="rounded-lg px-4 py-2 text-sm text-gray-600 hover:bg-gray-100 dark:text-gray-400 dark:hover:bg-gray-700"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        type="button"
                        onClick={onConfirm}
                        className={`rounded-lg px-4 py-2 text-sm text-white ${confirmColorClass}`}
                    >
                        {confirmLabel || t('common.delete')}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default ConfirmOverlay;
