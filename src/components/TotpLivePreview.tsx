// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { KeyRound } from 'lucide-react';

interface TotpLivePreviewProps {
  /** Raw base32 secret as typed in the "TOTP Secret (saved)" field. */
  secret: string;
  t: (key: string, params?: Record<string, string | number>) => string;
}

interface TotpPreview {
  code: string;
  seconds_remaining: number;
}

/**
 * Diagnostic for issue #128: shows the 6-digit code AeroFTP derives from
 * the entered base32 secret, with a 30s countdown, so the user can confirm
 * it matches their authenticator app. It calls the exact same derivation
 * used at connect time, so a match here means a match at login.
 *
 * Deliberately string-free (icon + digits + countdown only) so it adds no
 * new i18n keys; only the already-translated `common.failed` is reused.
 */
export const TotpLivePreview: React.FC<TotpLivePreviewProps> = ({ secret, t }) => {
  const [code, setCode] = useState<string | null>(null);
  const [remaining, setRemaining] = useState(0);
  const [failed, setFailed] = useState(false);
  const reqIdRef = useRef(0);

  const trimmed = secret.trim();

  // Debounced fetch whenever the secret changes.
  useEffect(() => {
    if (!trimmed) {
      setCode(null);
      setFailed(false);
      return;
    }
    const myId = ++reqIdRef.current;
    const timer = setTimeout(async () => {
      try {
        const res = await invoke<TotpPreview>('preview_provider_totp', { secret: trimmed });
        if (reqIdRef.current !== myId) return;
        setCode(res.code);
        setRemaining(res.seconds_remaining);
        setFailed(false);
      } catch {
        if (reqIdRef.current !== myId) return;
        setCode(null);
        setFailed(true);
      }
    }, 400);
    return () => clearTimeout(timer);
  }, [trimmed]);

  // 1s countdown; refetch when the current code rolls over.
  useEffect(() => {
    if (!trimmed || code === null) return;
    const iv = setInterval(() => {
      setRemaining((r) => {
        if (r <= 1) {
          const myId = ++reqIdRef.current;
          invoke<TotpPreview>('preview_provider_totp', { secret: trimmed })
            .then((res) => {
              if (reqIdRef.current !== myId) return;
              setCode(res.code);
              setRemaining(res.seconds_remaining);
              setFailed(false);
            })
            .catch(() => {
              if (reqIdRef.current !== myId) return;
              setCode(null);
              setFailed(true);
            });
          return 0;
        }
        return r - 1;
      });
    }, 1000);
    return () => clearInterval(iv);
  }, [trimmed, code]);

  if (!trimmed) return null;

  if (failed) {
    return (
      <div className="mt-1.5 flex items-center gap-1.5 text-xs text-amber-600 dark:text-amber-400">
        <KeyRound size={13} />
        <span>{t('common.failed')}</span>
      </div>
    );
  }

  if (code === null) return null;

  // Fraction of the 30s window left, for the shrinking ring.
  const pct = Math.max(0, Math.min(100, (remaining / 30) * 100));

  return (
    <div className="mt-1.5 flex items-center gap-2 text-xs">
      <KeyRound size={13} className="text-emerald-600 dark:text-emerald-400 flex-shrink-0" />
      <span className="font-mono text-base tracking-[0.3em] text-gray-800 dark:text-gray-100 select-all">
        {code}
      </span>
      <span
        className="relative inline-flex items-center justify-center w-6 h-6 flex-shrink-0"
        title={`${remaining}s`}
        aria-label={`${remaining}s`}
      >
        <svg viewBox="0 0 36 36" className="w-6 h-6 -rotate-90">
          <circle cx="18" cy="18" r="15" fill="none" strokeWidth="3"
            className="stroke-gray-200 dark:stroke-gray-700" />
          <circle cx="18" cy="18" r="15" fill="none" strokeWidth="3"
            strokeLinecap="round"
            className={remaining <= 5
              ? 'stroke-amber-500'
              : 'stroke-emerald-500'}
            strokeDasharray={2 * Math.PI * 15}
            strokeDashoffset={(2 * Math.PI * 15) * (1 - pct / 100)}
          />
        </svg>
        <span className="absolute text-[10px] font-medium text-gray-600 dark:text-gray-300">
          {remaining}
        </span>
      </span>
    </div>
  );
};
