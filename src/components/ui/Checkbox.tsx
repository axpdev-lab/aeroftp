// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

interface CheckboxProps {
    checked: boolean;
    onChange: (checked: boolean) => void;
    id?: string;
    disabled?: boolean;
    className?: string;
    label?: React.ReactNode;
    labelClassName?: string;
    'aria-label'?: string;
}

/**
 * Custom checkbox with animated SVG checkmark.
 * Drop-in replacement for `<input type="checkbox">` + `<label>`.
 *
 * Usage:
 *   <Checkbox checked={val} onChange={setVal} label="Save connection" />
 *   <Checkbox checked={val} onChange={setVal} id="my-cb" />
 */
export const Checkbox: React.FC<CheckboxProps> = React.memo(({
    checked,
    onChange,
    id,
    disabled = false,
    className = '',
    label,
    labelClassName = '',
    'aria-label': ariaLabel,
}) => {
    const handleClick = React.useCallback(() => {
        if (!disabled) onChange(!checked);
    }, [checked, disabled, onChange]);

    const handleKeyDown = React.useCallback((e: React.KeyboardEvent) => {
        if (e.key === ' ' || e.key === 'Enter') {
            e.preventDefault();
            if (!disabled) onChange(!checked);
        }
    }, [checked, disabled, onChange]);

    const box = (
        <span
            role="checkbox"
            aria-checked={checked}
            aria-disabled={disabled}
            aria-label={ariaLabel}
            tabIndex={disabled ? -1 : 0}
            id={id}
            onKeyDown={handleKeyDown}
            className={`
                relative inline-flex items-center justify-center w-4 h-4 shrink-0
                rounded border transition-all duration-150 outline-none
                focus-visible:ring-2 focus-visible:ring-blue-400 focus-visible:ring-offset-1 focus-visible:ring-offset-white dark:focus-visible:ring-offset-gray-900
                ${checked
                    ? 'bg-blue-500 border-blue-500'
                    : 'bg-transparent border-gray-400 dark:border-gray-500'
                }
                ${disabled
                    ? 'opacity-40 cursor-not-allowed'
                    : 'cursor-pointer hover:border-blue-400'
                }
                ${className}
            `}
        >
            {/* Animated checkmark SVG */}
            <svg
                viewBox="0 0 12 12"
                fill="none"
                className={`w-2.5 h-2.5 transition-all duration-150 ${
                    checked ? 'opacity-100 scale-100' : 'opacity-0 scale-75'
                }`}
            >
                <path
                    d="M2 6.5L4.5 9L10 3"
                    stroke="white"
                    strokeWidth="2"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                />
            </svg>
        </span>
    );

    if (!label) {
        return <span onClick={handleClick}>{box}</span>;
    }

    return (
        <label
            className={`inline-flex items-center gap-2 select-none ${disabled ? 'cursor-not-allowed' : 'cursor-pointer'} ${labelClassName}`}
            onClick={(e) => {
                // Prevent double-fire if clicking the box itself
                if ((e.target as HTMLElement).getAttribute('role') === 'checkbox') return;
                handleClick();
            }}
        >
            {box}
            {label}
        </label>
    );
});

Checkbox.displayName = 'Checkbox';
