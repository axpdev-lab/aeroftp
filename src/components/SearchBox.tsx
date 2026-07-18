// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Search, X } from 'lucide-react';

export interface SearchBoxProps {
    /** Current search text (controlled). */
    value: string;
    /** Called with the new string value (not the raw event). */
    onChange: (value: string) => void;
    placeholder?: string;
    /**
     * Classes applied to the <input> itself (bg, border, focus ring, text size,
     * padding). Keep each call site's original input classes here — but the old
     * `pl-9` left-padding hack is no longer needed: the 🔍 icon now sits OUTSIDE
     * to the left of the field, so a normal `pl-3` is enough.
     */
    className?: string;
    /**
     * Classes applied to the outer wrapper (layout: width / margins / flex).
     * e.g. `max-w-md`, `flex-1`, `mb-3`, `px-2 pb-1`.
     */
    containerClassName?: string;
    /** Classes for the search icon (colour). Defaults to a muted gray. */
    iconClassName?: string;
    /** Search icon size in px. Default 14. */
    iconSize?: number;
    autoFocus?: boolean;
    onKeyDown?: React.KeyboardEventHandler<HTMLInputElement>;
    /**
     * When provided, a clear (✕) button is rendered inside the field while
     * `value` is non-empty; clicking it invokes this callback.
     */
    onClear?: () => void;
    /** aria-label for the clear button (omitted when absent). */
    clearAriaLabel?: string;
    /** Clear (✕) icon size in px. Default 14. */
    clearIconSize?: number;
    /** Forwarded to the underlying <input> (focus management at call sites). */
    inputRef?: React.Ref<HTMLInputElement>;
    /** aria-label for the input. Falls back to `placeholder`. */
    ariaLabel?: string;
    type?: string;
}

/**
 * Shared search field. The magnifier icon lives OUTSIDE the input, to its
 * left, as a flex sibling — so it can never overlap the placeholder or the
 * typed text (the bug behind EF-24). The input is narrowed from the left by
 * the icon + gap; it keeps its own comfortable left padding instead of the old
 * absolutely-positioned-icon `pl-9` reservation.
 */
export const SearchBox: React.FC<SearchBoxProps> = ({
    value,
    onChange,
    placeholder,
    className = '',
    containerClassName = '',
    iconClassName = 'text-gray-400',
    iconSize = 14,
    autoFocus,
    onKeyDown,
    onClear,
    clearAriaLabel,
    clearIconSize = 14,
    inputRef,
    ariaLabel,
    type = 'text',
}) => {
    const showClear = !!onClear && value.length > 0;
    return (
        <div className={`flex items-center gap-2 ${containerClassName}`.trim()}>
            <Search size={iconSize} className={`shrink-0 ${iconClassName}`.trim()} />
            <div className="relative flex-1 min-w-0">
                <input
                    ref={inputRef}
                    type={type}
                    value={value}
                    onChange={(e) => onChange(e.target.value)}
                    onKeyDown={onKeyDown}
                    autoFocus={autoFocus}
                    placeholder={placeholder}
                    aria-label={ariaLabel ?? placeholder}
                    className={className}
                />
                {showClear && (
                    <button
                        type="button"
                        onClick={onClear}
                        aria-label={clearAriaLabel}
                        className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                    >
                        <X size={clearIconSize} />
                    </button>
                )}
            </div>
        </div>
    );
};

export default SearchBox;
