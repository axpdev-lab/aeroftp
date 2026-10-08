// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { Check, Copy } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useClipboardCopy } from '../../hooks/useClipboardCopy';

type TextField = HTMLInputElement | HTMLTextAreaElement;
const textTypes = new Set(['text', 'email', 'url', 'tel', 'search']);

function canCopy(field: Element | null): field is TextField {
    return (field instanceof HTMLTextAreaElement || (field instanceof HTMLInputElement && textTypes.has(field.type)))
        && !field.disabled && !!field.value;
}

/** One hover/focus affordance for the varied provider forms. Password inputs
 * become eligible only when their own eye toggle changes them to type=text.
 * Values are read at click time and never put into tooltips or React state. */
export function CopyableFormFields({ enabled, children, className }: {
    enabled: boolean;
    children: React.ReactNode;
    className?: string;
}) {
    const t = useTranslation();
    const root = useRef<HTMLDivElement>(null);
    const button = useRef<HTMLButtonElement>(null);
    const [field, setField] = useState<TextField | null>(null);
    const [position, setPosition] = useState({ left: 0, top: 0 });
    const [copiedField, setCopiedField] = useState<TextField | null>(null);
    const { copy, copied } = useClipboardCopy();
    const visible = enabled && canCopy(field);

    useEffect(() => { if (!enabled) setField(null); }, [enabled]);

    useLayoutEffect(() => {
        if (!visible || !field || !root.current) return;
        // React has just committed input value/type changes. A newly hidden
        // password or cleared field must lose its copy button before paint.
        if (!canCopy(field)) { setField(null); return; }
        const originalPadding = field.style.paddingRight;
        const computed = getComputedStyle(field);
        const padding = parseFloat(computed.paddingRight) || 0;
        const plainPadding = parseFloat(computed.paddingLeft) || 0;
        const inset = padding > plainPadding + 4 ? padding : 6;
        // Reserve room so a long access key remains readable next to Copy.
        // The original padding still determines where the eye buttons sit.
        field.style.paddingRight = `${padding + 30}px`;
        const reposition = () => {
            if (!root.current || !field.isConnected) { setField(null); return; }
            const outer = root.current.getBoundingClientRect();
            const inner = field.getBoundingClientRect();
            // Place before the eye/generator buttons when the field reserves
            // trailing space for them; otherwise use the inside-right edge.
            const next = {
                left: inner.right - outer.left - inset - 26,
                top: inner.top - outer.top + (inner.height - 26) / 2,
            };
            setPosition(previous => previous.left === next.left && previous.top === next.top ? previous : next);
        };
        reposition();
        window.addEventListener('resize', reposition);
        window.addEventListener('scroll', reposition, true);
        const observer = typeof ResizeObserver === 'undefined' ? undefined : new ResizeObserver(reposition);
        observer?.observe(field);
        observer?.observe(root.current);
        return () => {
            field.style.paddingRight = originalPadding;
            window.removeEventListener('resize', reposition);
            window.removeEventListener('scroll', reposition, true);
            observer?.disconnect();
        };
    }, [visible, field, children]);

    const enter = (target: EventTarget) => {
        if (!enabled || !(target instanceof Element) || button.current?.contains(target)) return;
        // Existing explicit copy controls (e.g. revealed Crypt secrets) already
        // cover their input, so do not place a second button over those fields.
        const existingCopy = target.parentElement?.querySelector('button[aria-label]');
        if (canCopy(target) && existingCopy?.getAttribute('aria-label') !== t('common.copy')) setField(target);
    };
    const leave = (next: EventTarget | null) => {
        if (next === field || (next instanceof Node && button.current?.contains(next))) return;
        if (document.activeElement !== field) setField(null);
    };

    return (
        <div
            ref={root}
            className={className}
            onPointerOver={event => enter(event.target)}
            onFocusCapture={event => enter(event.target)}
            onPointerOut={event => leave(event.relatedTarget)}
            onBlurCapture={event => leave(event.relatedTarget)}
        >
            {children}
            {visible && (
                <button
                    ref={button}
                    type="button"
                    data-agent="deny"
                    aria-label={t('common.copy')}
                    title={copied && copiedField === field ? t('common.copied') : t('common.copy')}
                    className="absolute z-20 w-[26px] h-[26px] flex items-center justify-center rounded bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-600 shadow-sm text-gray-400 hover:text-blue-500 focus-visible:ring-2 focus-visible:ring-blue-500"
                    style={position}
                    onMouseDown={event => event.preventDefault()}
                    onClick={async event => {
                        event.preventDefault();
                        event.stopPropagation();
                        if (enabled && canCopy(field) && await copy(field.value)) setCopiedField(field);
                    }}
                >
                    {copied && copiedField === field ? <Check size={14} className="text-green-500" /> : <Copy size={14} />}
                </button>
            )}
        </div>
    );
}
