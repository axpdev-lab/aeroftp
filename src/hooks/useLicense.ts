import { createContext, useContext, useState, useEffect, useCallback, useRef, type ReactNode } from 'react';
import { invoke } from '@tauri-apps/api/core';
import React from 'react';

export interface LicenseStatus {
  is_pro: boolean;
  tier: string;
  license_id: string | null;
  activated_at: number | null;
  grace_period_remaining_days: number | null;
}

interface LicenseContextValue {
  isPro: boolean;
  tier: 'free' | 'pro';
  licenseId: string | null;
  loading: boolean;
  graceDaysRemaining: number | null;
  humanReadableKey: string | null;
  activate: (token: string) => Promise<LicenseStatus>;
  deactivate: () => Promise<void>;
  refresh: () => Promise<void>;
}

const defaultValue: LicenseContextValue = {
  isPro: false,
  tier: 'free',
  licenseId: null,
  loading: true,
  graceDaysRemaining: null,
  humanReadableKey: null,
  activate: async () => ({ is_pro: false, tier: 'free', license_id: null, activated_at: null, grace_period_remaining_days: null }),
  deactivate: async () => {},
  refresh: async () => {},
};

const LicenseContext = createContext<LicenseContextValue>(defaultValue);

export function useLicense(): LicenseContextValue {
  return useContext(LicenseContext);
}

function useLicenseProvider(): LicenseContextValue {
  const [isPro, setIsPro] = useState(false);
  const [tier, setTier] = useState<'free' | 'pro'>('free');
  const [licenseId, setLicenseId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [graceDaysRemaining, setGraceDaysRemaining] = useState<number | null>(null);
  const [humanReadableKey, setHumanReadableKey] = useState<string | null>(null);
  const mountedRef = useRef(true);

  const applyStatus = useCallback((status: LicenseStatus) => {
    if (!mountedRef.current) return;
    setIsPro(status.is_pro);
    setTier(status.is_pro ? 'pro' : 'free');
    setLicenseId(status.license_id);
    setGraceDaysRemaining(status.grace_period_remaining_days);
  }, []);

  const refresh = useCallback(async () => {
    try {
      const status = await invoke<LicenseStatus>('license_check');
      applyStatus(status);
      if (status.is_pro) {
        const key = await invoke<string | null>('license_get_key');
        if (mountedRef.current) setHumanReadableKey(key);
      } else {
        if (mountedRef.current) setHumanReadableKey(null);
      }
    } catch {
      if (mountedRef.current) {
        setIsPro(false);
        setTier('free');
        setLicenseId(null);
        setHumanReadableKey(null);
      }
    } finally {
      if (mountedRef.current) setLoading(false);
    }
  }, [applyStatus]);

  const activate = useCallback(async (token: string): Promise<LicenseStatus> => {
    const status = await invoke<LicenseStatus>('license_activate', { token });
    applyStatus(status);
    if (status.is_pro) {
      const key = await invoke<string | null>('license_get_key');
      if (mountedRef.current) setHumanReadableKey(key);
    }
    return status;
  }, [applyStatus]);

  const deactivate = useCallback(async () => {
    await invoke('license_deactivate');
    if (mountedRef.current) {
      setIsPro(false);
      setTier('free');
      setLicenseId(null);
      setGraceDaysRemaining(null);
      setHumanReadableKey(null);
    }
  }, []);

  useEffect(() => {
    mountedRef.current = true;
    refresh();
    return () => { mountedRef.current = false; };
  }, [refresh]);

  return {
    isPro,
    tier,
    licenseId,
    loading,
    graceDaysRemaining,
    humanReadableKey,
    activate,
    deactivate,
    refresh,
  };
}

export function LicenseProvider({ children }: { children: ReactNode }) {
  const value = useLicenseProvider();
  return React.createElement(LicenseContext.Provider, { value }, children);
}
