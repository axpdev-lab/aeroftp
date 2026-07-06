// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useRef } from 'react';

/**
 * Shared 6-box TOTP code input (#369 / Ehud): one single-digit box per digit,
 * digit-only, with auto-advance, backspace-to-previous, arrow navigation and
 * full-code paste. Purely presentational: it is a controlled view over the same
 * plain string value the single-field input used, and emits that same string
 * through `onChange`, so the downstream 2FA auth path is unchanged.
 *
 * The emitted value is digits-only, capped at `length` (default 6). Reused on
 * every Quick Connect page that takes a one-time TOTP code (MEGA, Filen,
 * Internxt), so the styling stays consistent.
 */
interface TotpCodeInputProps {
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  autoFocus?: boolean;
  length?: number;
}

export const TotpCodeInput: React.FC<TotpCodeInputProps> = ({
  value,
  onChange,
  disabled,
  autoFocus,
  length = 6,
}) => {
  const inputsRef = useRef<Array<HTMLInputElement | null>>([]);
  const digits = (value || '').replace(/\D/g, '').slice(0, length);

  const focusBox = (idx: number) => {
    const clamped = Math.max(0, Math.min(length - 1, idx));
    inputsRef.current[clamped]?.focus();
    inputsRef.current[clamped]?.select();
  };

  // Overwrite starting at `idx` with `incoming` digits, emit the joined code.
  const fillFrom = (idx: number, incoming: string) => {
    const arr = Array.from({ length }, (_, k) => digits[k] || '');
    let p = idx;
    for (const ch of incoming) {
      if (p >= length) break;
      arr[p++] = ch;
    }
    onChange(arr.join('').replace(/\D/g, '').slice(0, length));
    return Math.min(p, length - 1);
  };

  const handleChange = (idx: number, raw: string) => {
    const only = raw.replace(/\D/g, '');
    if (!only) {
      // Cleared the box.
      const arr = Array.from({ length }, (_, k) => digits[k] || '');
      arr[idx] = '';
      onChange(arr.join('').replace(/\D/g, '').slice(0, length));
      return;
    }
    const landed = fillFrom(idx, only);
    if (only.length >= 1) focusBox(landed + (only.length === 1 ? 1 : 0));
  };

  const handleKeyDown = (idx: number, e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Backspace') {
      const arr = Array.from({ length }, (_, k) => digits[k] || '');
      if (arr[idx]) {
        arr[idx] = '';
        onChange(arr.join('').replace(/\D/g, '').slice(0, length));
      } else if (idx > 0) {
        e.preventDefault();
        arr[idx - 1] = '';
        onChange(arr.join('').replace(/\D/g, '').slice(0, length));
        focusBox(idx - 1);
      }
    } else if (e.key === 'ArrowLeft') {
      e.preventDefault();
      focusBox(idx - 1);
    } else if (e.key === 'ArrowRight') {
      e.preventDefault();
      focusBox(idx + 1);
    }
  };

  const handlePaste = (idx: number, e: React.ClipboardEvent<HTMLInputElement>) => {
    const only = e.clipboardData.getData('text').replace(/\D/g, '');
    if (!only) return;
    e.preventDefault();
    const landed = fillFrom(idx, only);
    focusBox(landed);
  };

  return (
    <div className="flex gap-2" role="group" aria-label="6-digit code">
      {Array.from({ length }).map((_, i) => (
        <input
          key={i}
          ref={(el) => { inputsRef.current[i] = el; }}
          type="text"
          inputMode="numeric"
          autoComplete="one-time-code"
          maxLength={1}
          disabled={disabled}
          autoFocus={autoFocus && i === 0}
          value={digits[i] || ''}
          onChange={(e) => handleChange(i, e.target.value)}
          onKeyDown={(e) => handleKeyDown(i, e)}
          onPaste={(e) => handlePaste(i, e)}
          onFocus={(e) => e.target.select()}
          className="totp-box w-10 h-11 text-center font-mono text-lg bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 disabled:opacity-50"
          aria-label={`Digit ${i + 1}`}
        />
      ))}
    </div>
  );
};
