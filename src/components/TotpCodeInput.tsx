// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useRef, useState, useEffect } from 'react';

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
 *
 * State is held PER BOX internally (not derived from the joined string) so that
 * clearing a middle box and retyping corrects that box in place instead of
 * shifting every later digit one box left. The compacted digits-only string is
 * ONLY the emitted wire format: a code with a hole is incomplete and cannot be
 * submitted, so it is fine for holes to compact away in the emitted value while
 * the visible box positions are preserved for the correction.
 */
interface TotpCodeInputProps {
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  autoFocus?: boolean;
  length?: number;
}

// --- Pure helpers (exported for tests; the component body uses these) --------

/** Digits-only, capped at `length`. This is the emitted wire format shape. */
export function normalizeCode(value: string, length: number): string {
  return (value || '').replace(/\D/g, '').slice(0, length);
}

/** Left-aligned per-box array of exactly `length` slots ('' for empty boxes). */
export function splitCode(value: string, length: number): string[] {
  const code = normalizeCode(value, length);
  return Array.from({ length }, (_, k) => code[k] || '');
}

/** Join boxes into the emitted digits-only string (holes compact away here). */
export function joinBoxes(boxes: string[]): string {
  return boxes.join('').replace(/\D/g, '');
}

/**
 * Overwrite boxes starting at `idx` with `incoming` (non-digits filtered),
 * capped at the box count. Returns the new boxes plus `landed`, the box index
 * to focus after the write (the slot past the last written digit, clamped).
 */
export function writeDigits(
  boxes: string[],
  idx: number,
  incoming: string,
): { boxes: string[]; landed: number } {
  const length = boxes.length;
  const only = (incoming || '').replace(/\D/g, '');
  const next = boxes.slice();
  let p = idx;
  for (const ch of only) {
    if (p >= length) break;
    next[p++] = ch;
  }
  return { boxes: next, landed: Math.min(p, length - 1) };
}

/** Clear a single box in place, leaving every other box untouched. */
export function clearBoxAt(boxes: string[], idx: number): string[] {
  const next = boxes.slice();
  if (idx >= 0 && idx < next.length) next[idx] = '';
  return next;
}

// -----------------------------------------------------------------------------

export const TotpCodeInput: React.FC<TotpCodeInputProps> = ({
  value,
  onChange,
  disabled,
  autoFocus,
  length = 6,
}) => {
  const inputsRef = useRef<Array<HTMLInputElement | null>>([]);
  const [boxes, setBoxes] = useState<string[]>(() => splitCode(value, length));

  // Resync from the parent ONLY when its value diverges from what we last
  // emitted, so external resets (a cleared field after a failed login, or a
  // programmatically set code) apply while our own emissions never clobber the
  // in-place hole positions we keep for corrections.
  useEffect(() => {
    setBoxes((prev) =>
      joinBoxes(prev) === normalizeCode(value, length) ? prev : splitCode(value, length),
    );
  }, [value, length]);

  const focusBox = (idx: number) => {
    const clamped = Math.max(0, Math.min(length - 1, idx));
    inputsRef.current[clamped]?.focus();
    inputsRef.current[clamped]?.select();
  };

  const emit = (next: string[]) => {
    setBoxes(next);
    onChange(joinBoxes(next));
  };

  const handleChange = (idx: number, raw: string) => {
    const only = raw.replace(/\D/g, '');
    if (!only) {
      // Cleared the box: leave every later box where it is (no compaction).
      emit(clearBoxAt(boxes, idx));
      return;
    }
    // writeDigits returns the next box index (p advanced past the written
    // digit), so focus it directly. Same as handlePaste; adding +1 here would
    // double-advance and skip a box on every single-digit keystroke.
    const { boxes: next, landed } = writeDigits(boxes, idx, only);
    emit(next);
    focusBox(landed);
  };

  const handleKeyDown = (idx: number, e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Backspace') {
      if (boxes[idx]) {
        emit(clearBoxAt(boxes, idx));
      } else if (idx > 0) {
        e.preventDefault();
        emit(clearBoxAt(boxes, idx - 1));
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
    const { boxes: next, landed } = writeDigits(boxes, idx, only);
    emit(next);
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
          value={boxes[i] || ''}
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
