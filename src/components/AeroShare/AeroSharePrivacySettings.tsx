// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * AeroSharePrivacySettings - the privacy + anti-flood controls for AeroShare.
 *
 * The v4.1.0 security follow-ups (#370). Source of truth is the per-partition
 * backend store (`peer_settings` / `peer_mutes`), NOT the frontend AppSettings,
 * because the receive loop reads these live to gate inbound knocks/actions/file
 * offers. So this panel loads/saves directly through the `peerSettings*` /
 * `peerMutes*` commands instead of the AppSettings store. Three controls:
 *
 *  - Friends-only gate: accept inbound signals only from saved contacts (the
 *    stranger attention-DoS mitigation; OFF by default to keep first contact).
 *  - Rate limit: max inbound signals per sender per minute (0 = off).
 *  - Discovery mode: opt the long-term AFID out of the public DHT (the Info
 *    finding hardening), offering every mode so users pick reachability vs
 *    privacy for their own need.
 *
 * Plus the muted-senders list (unmute) and the destructive AFID rotation.
 */

import { useCallback, useEffect, useState } from 'react';
import { Loader2, ShieldOff, RefreshCw, BellOff, AlertTriangle } from 'lucide-react';
import { useTranslation } from '../../i18n';
import {
  peerSettingsGet,
  peerSettingsSet,
  peerMutesList,
  peerContactUnmute,
  peerIdentityRotate,
  shortAfid,
  type PeerSettings,
  type PeerDiscoveryMode,
} from '../../utils/aeroShare';

const DISCOVERY_MODES: PeerDiscoveryMode[] = ['both', 'dht', 'n0', 'lan', 'none'];

export function AeroSharePrivacySettings() {
  const t = useTranslation();

  const [settings, setSettings] = useState<PeerSettings | null>(null);
  const [mutes, setMutes] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  // AFID rotation: a two-step confirm (destructive) + the freshly minted AFID.
  const [confirmRotate, setConfirmRotate] = useState(false);
  const [rotatedAfid, setRotatedAfid] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [s, m] = await Promise.all([peerSettingsGet(), peerMutesList()]);
      setSettings(s);
      setMutes(m);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Persist a patch, optimistically updating local state so the control reflects
  // the change at once; reloads from the backend on failure to stay truthful.
  const patch = useCallback(
    async (next: PeerSettings) => {
      setBusy(true);
      setSettings(next);
      try {
        await peerSettingsSet(next);
        setError(null);
      } catch (e) {
        setError(String(e));
        await refresh();
      } finally {
        setBusy(false);
      }
    },
    [refresh],
  );

  const unmute = useCallback(
    async (afid: string) => {
      setBusy(true);
      try {
        await peerContactUnmute(afid);
        setMutes((cur) => cur.filter((a) => a !== afid));
      } catch (e) {
        setError(String(e));
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const rotate = useCallback(async () => {
    setBusy(true);
    setConfirmRotate(false);
    try {
      const afid = await peerIdentityRotate();
      setRotatedAfid(afid);
      setError(null);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  if (loading || !settings) {
    return (
      <div className="flex items-center gap-2 py-6 justify-center text-gray-400">
        <Loader2 size={16} className="animate-spin" />
      </div>
    );
  }

  const selectCls =
    'w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white ' +
    'dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm disabled:opacity-50';

  return (
    <div className="space-y-4">
      {error && (
        <div className="px-3 py-2 text-xs rounded-md bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 border border-red-200 dark:border-red-800">
          {error}
        </div>
      )}

      {/* Friends-only gate */}
      <label className="flex items-start gap-2 cursor-pointer">
        <input
          type="checkbox"
          className="mt-1"
          checked={settings.friendsOnly}
          disabled={busy}
          onChange={(e) => patch({ ...settings, friendsOnly: e.target.checked })}
        />
        <div>
          <p className="font-medium flex items-center gap-2">
            <ShieldOff size={15} className="text-violet-500" />
            {t('aeroShare.privacy.friendsOnlyLabel')}
          </p>
          <p className="text-sm text-gray-500">{t('aeroShare.privacy.friendsOnlyDesc')}</p>
        </div>
      </label>

      {/* Rate limit */}
      <div>
        <label className="block text-sm font-medium mb-1">
          {t('aeroShare.privacy.rateLimitLabel')}
        </label>
        <div className="flex items-center gap-2">
          <input
            type="number"
            min={0}
            max={1000}
            className={`${selectCls} max-w-[8rem]`}
            value={settings.rateLimitPerMin}
            disabled={busy}
            onChange={(e) => {
              const n = Math.max(0, Math.min(1000, Math.floor(Number(e.target.value) || 0)));
              patch({ ...settings, rateLimitPerMin: n });
            }}
          />
          <span className="text-sm text-gray-500">{t('aeroShare.privacy.rateLimitUnit')}</span>
        </div>
        <p className="text-xs text-gray-500 mt-1">
          {settings.rateLimitPerMin === 0
            ? t('aeroShare.privacy.rateLimitOff')
            : t('aeroShare.privacy.rateLimitDesc')}
        </p>
      </div>

      {/* Discovery mode */}
      <div>
        <label className="block text-sm font-medium mb-1">
          {t('aeroShare.privacy.discoveryLabel')}
        </label>
        <select
          className={selectCls}
          value={settings.discoveryMode}
          disabled={busy}
          onChange={(e) => patch({ ...settings, discoveryMode: e.target.value as PeerDiscoveryMode })}
        >
          {DISCOVERY_MODES.map((m) => (
            <option key={m} value={m}>
              {t(`aeroShare.privacy.discovery_${m}`)}
            </option>
          ))}
        </select>
        <p className="text-xs text-gray-500 mt-1">
          {t(`aeroShare.privacy.discovery_${settings.discoveryMode}_desc`)}
        </p>
      </div>

      {/* Muted senders */}
      <div className="border-t border-gray-200 dark:border-gray-700 pt-4">
        <p className="font-medium flex items-center gap-2 mb-2">
          <BellOff size={15} className="text-violet-500" />
          {t('aeroShare.privacy.mutedTitle')}
        </p>
        {mutes.length === 0 ? (
          <p className="text-xs text-gray-500">{t('aeroShare.privacy.mutedEmpty')}</p>
        ) : (
          <ul className="divide-y divide-gray-100 dark:divide-gray-700/60">
            {mutes.map((afid) => (
              <li key={afid} className="flex items-center gap-2 py-2">
                <span className="min-w-0 flex-1 text-[11px] font-mono text-gray-500 dark:text-gray-400 truncate">
                  {shortAfid(afid)}
                </span>
                <button
                  onClick={() => unmute(afid)}
                  disabled={busy}
                  className="px-2.5 py-1 text-xs font-medium rounded-md border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
                >
                  {t('aeroShare.privacy.unmute')}
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>

      {/* AFID rotation (destructive) */}
      <div className="border-t border-gray-200 dark:border-gray-700 pt-4">
        <p className="font-medium flex items-center gap-2 mb-1">
          <RefreshCw size={15} className="text-violet-500" />
          {t('aeroShare.privacy.rotateTitle')}
        </p>
        <p className="text-sm text-gray-500 mb-2">{t('aeroShare.privacy.rotateDesc')}</p>
        {rotatedAfid ? (
          <p className="text-xs text-green-600 dark:text-green-400 font-mono break-all">
            {t('aeroShare.privacy.rotateDone')} {shortAfid(rotatedAfid)}
          </p>
        ) : confirmRotate ? (
          <div className="rounded-md border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/20 p-3 space-y-2">
            <p className="text-xs text-amber-700 dark:text-amber-300 flex items-start gap-2">
              <AlertTriangle size={14} className="mt-0.5 shrink-0" />
              {t('aeroShare.privacy.rotateConfirm')}
            </p>
            <div className="flex gap-2">
              <button
                onClick={rotate}
                disabled={busy}
                className="px-3 py-1.5 text-xs font-medium rounded-md bg-red-600 text-white hover:bg-red-700 transition-colors disabled:opacity-50"
              >
                {t('aeroShare.privacy.rotateConfirmYes')}
              </button>
              <button
                onClick={() => setConfirmRotate(false)}
                disabled={busy}
                className="px-3 py-1.5 text-xs font-medium rounded-md border border-gray-300 dark:border-gray-600 text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
              >
                {t('aeroShare.privacy.rotateCancel')}
              </button>
            </div>
          </div>
        ) : (
          <button
            onClick={() => setConfirmRotate(true)}
            disabled={busy}
            className="px-3 py-1.5 text-xs font-medium rounded-md border border-red-300 dark:border-red-700 text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors disabled:opacity-50"
          >
            {t('aeroShare.privacy.rotateButton')}
          </button>
        )}
      </div>
    </div>
  );
}
