/**
 * OAuthConnect Component
 * Handles OAuth2 authentication for cloud providers (Google Drive, Dropbox, OneDrive)
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { ExternalLink, LogIn, CheckCircle, AlertCircle, Loader2, Settings, FolderOpen, Save, LogOut, RefreshCw, Eye, EyeOff } from 'lucide-react';
import { useOAuth2, OAuthProvider, OAUTH_APPS } from '../hooks/useOAuth2';
import { useI18n } from '../i18n';
import { openUrl } from '../utils/openUrl';
import { logger } from '../utils/logger';

interface OAuthConnectProps {
  provider: 'googledrive' | 'dropbox' | 'onedrive' | 'box' | 'pcloud' | 'zohoworkdrive';
  onConnected: (displayName: string, extraOptions?: { region?: string }) => void;
  disabled?: boolean;
  initialLocalPath?: string;
  onLocalPathChange?: (path: string) => void;
  saveConnection?: boolean;
  onSaveConnectionChange?: (save: boolean) => void;
  connectionName?: string;
  onConnectionNameChange?: (name: string) => void;
}

// Map our ProviderType to OAuthProvider
const providerMap: Record<string, OAuthProvider> = {
  googledrive: 'google_drive',
  dropbox: 'dropbox',
  onedrive: 'onedrive',
  box: 'box',
  pcloud: 'pcloud',
  zohoworkdrive: 'zoho_workdrive',
};

const providerNames: Record<string, string> = {
  googledrive: 'Google Drive',
  dropbox: 'Dropbox',
  onedrive: 'OneDrive',
  box: 'Box',
  pcloud: 'pCloud',
  zohoworkdrive: 'Zoho WorkDrive',
};

const providerColors: Record<string, string> = {
  googledrive: 'bg-red-500 hover:bg-red-600',
  dropbox: 'bg-blue-500 hover:bg-blue-600',
  onedrive: 'bg-sky-500 hover:bg-sky-600',
  box: 'bg-blue-600 hover:bg-blue-700',
  pcloud: 'bg-green-500 hover:bg-green-600',
  zohoworkdrive: 'bg-blue-700 hover:bg-blue-800',
};

// Zoho region options for multi-region support
const ZOHO_REGIONS = [
  { value: 'us', label: 'US (zoho.com)' },
  { value: 'eu', label: 'EU (zoho.eu)' },
  { value: 'in', label: 'India (zoho.in)' },
  { value: 'au', label: 'Australia (zoho.com.au)' },
  { value: 'jp', label: 'Japan (zoho.jp)' },
  { value: 'uk', label: 'UK (zoho.uk)' },
  { value: 'ca', label: 'Canada (zohocloud.ca)' },
  { value: 'sa', label: 'Saudi Arabia (zoho.sa)' },
];

// Provider icons as SVG components (white fill for buttons)
const ProviderIcon: React.FC<{ provider: string; className?: string; white?: boolean }> = ({ provider, className = "w-5 h-5", white = false }) => {
  const size = 20;
  switch (provider) {
    case 'googledrive':
      return white ? (
        <svg className={className} width={size} height={size} viewBox="0 0 87.3 78" fill="currentColor">
          <path d="M6.6 66.85l3.85 6.65c.8 1.4 1.95 2.5 3.3 3.3l13.75-23.8H0c0 1.55.4 3.1 1.2 4.5l5.4 9.35z" />
          <path d="M43.65 25L29.9 1.2c-1.35.8-2.5 1.9-3.3 3.3L1.2 52.35c-.8 1.4-1.2 2.95-1.2 4.5h27.5L43.65 25z" />
          <path d="M73.55 76.8c1.35-.8 2.5-1.9 3.3-3.3l1.6-2.75 7.65-13.25c.8-1.4 1.2-2.95 1.2-4.5H59.85L73.55 76.8z" />
          <path d="M43.65 25L57.4 1.2c-1.35-.8-2.9-1.2-4.5-1.2H34.35c-1.6 0-3.15.45-4.45 1.2L43.65 25z" />
          <path d="M59.85 53H27.5L13.75 76.8c1.35.8 2.9 1.2 4.5 1.2h50.8c1.6 0 3.15-.45 4.5-1.2L59.85 53z" />
          <path d="M73.4 26.5l-12.7-22c-.8-1.4-1.95-2.5-3.3-3.3L43.65 25l16.2 28h27.45c0-1.55-.4-3.1-1.2-4.5l-12.7-22z" />
        </svg>
      ) : (
        <svg className={className} width={size} height={size} viewBox="0 0 87.3 78">
          <path fill="#0066da" d="M6.6 66.85l3.85 6.65c.8 1.4 1.95 2.5 3.3 3.3l13.75-23.8H0c0 1.55.4 3.1 1.2 4.5l5.4 9.35z" />
          <path fill="#00ac47" d="M43.65 25L29.9 1.2c-1.35.8-2.5 1.9-3.3 3.3L1.2 52.35c-.8 1.4-1.2 2.95-1.2 4.5h27.5L43.65 25z" />
          <path fill="#ea4335" d="M73.55 76.8c1.35-.8 2.5-1.9 3.3-3.3l1.6-2.75 7.65-13.25c.8-1.4 1.2-2.95 1.2-4.5H59.85L73.55 76.8z" />
          <path fill="#00832d" d="M43.65 25L57.4 1.2c-1.35-.8-2.9-1.2-4.5-1.2H34.35c-1.6 0-3.15.45-4.45 1.2L43.65 25z" />
          <path fill="#2684fc" d="M59.85 53H27.5L13.75 76.8c1.35.8 2.9 1.2 4.5 1.2h50.8c1.6 0 3.15-.45 4.5-1.2L59.85 53z" />
          <path fill="#ffba00" d="M73.4 26.5l-12.7-22c-.8-1.4-1.95-2.5-3.3-3.3L43.65 25l16.2 28h27.45c0-1.55-.4-3.1-1.2-4.5l-12.7-22z" />
        </svg>
      );
    case 'dropbox':
      return (
        <svg className={className} width={size} height={size} viewBox="0 0 43 40" fill={white ? "currentColor" : "#0061ff"}>
          <path d="M12.5 0L0 8.1l8.5 6.9 12.5-8.2L12.5 0zM0 22l12.5 8.1 8.5-6.8-12.5-8.2L0 22zm21 1.3l8.5 6.8L42 22l-8.5-6.9-12.5 8.2zm21-15.2L29.5 0 21 6.8l12.5 8.2L42 8.1zM21.1 24.4l-8.6 6.9-3.9-2.6v2.9l12.5 7.5 12.5-7.5v-2.9l-3.9 2.6-8.6-6.9z" />
        </svg>
      );
    case 'onedrive':
      return white ? (
        <svg className={className} width={size} height={size} viewBox="0 0 24 24" fill="currentColor">
          <path d="M19.35 10.04A7.49 7.49 0 0012 4C9.11 4 6.6 5.64 5.35 8.04A5.994 5.994 0 000 14c0 3.31 2.69 6 6 6h13c2.76 0 5-2.24 5-5 0-2.64-2.05-4.78-4.65-4.96z" />
        </svg>
      ) : (
        <svg className={className} width={size} height={size} viewBox="0 0 24 24">
          <path fill="#0364b8" d="M14.5 15h6.78l.72-.53V14c0-2.48-1.77-4.6-4.17-5.05A5.5 5.5 0 0 0 7.5 10.5v.5H7c-2.21 0-4 1.79-4 4s1.79 4 4 4h7.5z" />
          <path fill="#0078d4" d="M9.5 10.5A5.5 5.5 0 0 1 17.83 8.95 5.5 5.5 0 0 0 14.5 15H7c-2.21 0-4-1.79-4-4s1.79-4 4-4h.5v.5c0 1.66.74 3.15 1.9 4.15.4-.08.8-.15 1.1-.15z" />
          <path fill="#1490df" d="M21.28 14.47l-.78.53H14.5 7c-2.21 0-4-1.79-4-4a3.99 3.99 0 0 1 2.4-3.67A4 4 0 0 1 9 6c.88 0 1.7.29 2.36.78A5.49 5.49 0 0 1 17.83 9a5 5 0 0 1 3.45 5.47z" />
          <path fill="#28a8ea" d="M21.28 14.47A5 5 0 0 0 17.83 9a5.49 5.49 0 0 0-6.47-1.22A4 4 0 0 0 5.4 10.33c-.35.11-.68.28-.98.5a4.49 4.49 0 0 0 2.08 4.67H14.5h6.78z" />
        </svg>
      );
    case 'box':
      return (
        <svg className={className} width={size} height={size} viewBox="0 0 40 40" fill={white ? "currentColor" : "#0061D5"}>
          <g transform="translate(0, 9.2)">
            <path d="M39.7 19.2c.5.7.4 1.6-.2 2.1-.7.5-1.7.4-2.2-.2l-3.5-4.5-3.4 4.4c-.5.7-1.5.7-2.2.2-.7-.5-.8-1.4-.3-2.1l4-5.2-4-5.2c-.5-.7-.3-1.7.3-2.2.7-.5 1.7-.3 2.2.3l3.4 4.5L37.3 7c.5-.7 1.4-.8 2.2-.3.7.5.7 1.5.2 2.2L35.8 14l3.9 5.2zm-18.2-.6c-2.6 0-4.7-2-4.7-4.6 0-2.5 2.1-4.6 4.7-4.6s4.7 2.1 4.7 4.6c-.1 2.6-2.2 4.6-4.7 4.6zm-13.8 0c-2.6 0-4.7-2-4.7-4.6 0-2.5 2.1-4.6 4.7-4.6s4.7 2.1 4.7 4.6c0 2.6-2.1 4.6-4.7 4.6zM21.5 6.4c-2.9 0-5.5 1.6-6.8 4-1.3-2.4-3.9-4-6.9-4-1.8 0-3.4.6-4.7 1.5V1.5C3.1.7 2.4 0 1.6 0 .7 0 0 .7 0 1.5v12.6c.1 4.2 3.5 7.5 7.7 7.5 3 0 5.6-1.7 6.9-4.1 1.3 2.4 3.9 4.1 6.8 4.1 4.3 0 7.8-3.4 7.8-7.7.1-4.1-3.4-7.5-7.7-7.5z" />
          </g>
        </svg>
      );
    case 'pcloud':
      return (
        <svg className={className} width={size} height={size} viewBox="0 0 50 50" fill="none">
          <g transform="translate(0,9)">
            <path d="m 50,24 c 0,-2.5 -1.2,-4.8 -3,-6.2 -0.7,1.4 -2,2.6 -3.5,3.2 2.1,-1.1 3.6,-3.4 3.6,-6 0,-3.7 -3,-6.7 -6.7,-6.7 -0.3,0 -0.5,0 -0.8,0 0.9,2 1.4,4.2 1.4,6.6 0,0.2 0,0.3 0,0.5 C 40.7,6.9 33.7,0 25,0 16.3,0 9.3,6.9 9,15.4 9,15.3 9,15.1 9,15 9,12.6 9.5,10.4 10.4,8.4 4.5,9.2 0,14.1 0,20.2 0,26.7 5.4,32 11.9,32 H 42.1 C 46.5,31.9 50,28.4 50,24 Z" fill={white ? "currentColor" : "#17BED0"} />
            <circle cx="25" cy="16" r="11.2" fill="none" stroke={white ? "currentColor" : "#ffffff"} strokeWidth="1.6" />
            <text x="22" y="20.5" fill={white ? "currentColor" : "#ffffff"} fontSize="13" fontWeight="bold" fontFamily="Arial,sans-serif">P</text>
          </g>
        </svg>
      );
    case 'zohoworkdrive':
      return (
        <svg className={className} width={size} height={size} viewBox="0 0 24 24" fill={white ? "currentColor" : "#226DB4"} fillRule="evenodd">
          <path d="M21.2062 22H16.6624L16.6547 22L16.6468 22H7.02891C6.98027 22 6.93281 21.9951 6.88699 21.9858C6.56624 21.9209 6.32578 21.6401 6.32578 21.3023L6.32581 21.2963V19.7232C6.32581 18.9953 6.54612 18.2953 6.96565 17.6976C7.38518 17.1 7.96877 16.6511 8.65784 16.4L9.26336 16.1773C8.95723 15.8139 8.77271 15.3464 8.77271 14.8372C8.77271 13.6837 9.71958 12.7442 10.8821 12.7442C12.0446 12.7442 12.9915 13.6837 12.9915 14.8372C12.9915 14.8499 12.9913 14.8626 12.9911 14.8753C13.1503 14.7878 13.3171 14.712 13.4906 14.6488L13.836 14.5219C13.4815 14.1305 13.2656 13.6131 13.2656 13.0465C13.2656 11.8279 14.2641 10.8372 15.4922 10.8372C16.7203 10.8372 17.7188 11.8279 17.7188 13.0465C17.7188 13.0782 17.7181 13.1098 17.7167 13.1412C17.8505 13.0704 17.9897 13.0085 18.1336 12.9558L18.626 12.7749C18.2367 12.3661 17.9977 11.8148 17.9977 11.2092C17.9977 9.95105 19.0289 8.9278 20.2969 8.9278C21.5297 8.9278 22.5387 9.89503 22.5938 11.105V7.09766C22.5938 6.3372 21.9703 5.72092 21.2062 5.72092H12.2719C11.6742 5.72092 11.0789 5.52557 10.6008 5.16976L8.57344 3.66744C8.33437 3.49069 8.04141 3.39302 7.74375 3.39302H2.79375C2.02969 3.39535 1.40625 4.01395 1.40625 4.77209V19.2279C1.40625 19.9883 2.02969 20.6046 2.79375 20.6046H4.19297C4.58203 20.6046 4.89609 20.9162 4.89609 21.3023C4.89609 21.6883 4.58203 22 4.19297 22H2.79375C1.25391 22 0 20.7558 0 19.2279V4.77209C0 3.24418 1.25391 2 2.79375 2H7.74375C8.34141 2 8.93672 2.19535 9.41484 2.55116L11.4422 4.05348C11.6813 4.23023 11.9742 4.3279 12.2719 4.3279H21.2062C22.7461 4.3279 24 5.57209 24 7.09999V19.2302C24 20.7558 22.7461 22 21.2062 22ZM22.5938 11.3132V19.2279C22.5938 19.986 21.9727 20.6046 21.2062 20.6046H17.3601V16.0651C17.3601 15.2651 17.8687 14.5419 18.6234 14.2651L21.0726 13.3651C21.1091 13.3516 21.1437 13.3354 21.1765 13.3168C21.9785 12.9856 22.5526 12.216 22.5938 11.3132ZM15.9515 16.0628V20.6046H12.5672V17.972C12.5672 17.079 13.1344 16.2697 13.9804 15.9581L16.068 15.1915C15.9909 15.4729 15.9515 15.7652 15.9515 16.0628ZM15.7757 13.8103C16.0887 13.6956 16.3125 13.3965 16.3125 13.0465C16.3125 12.5977 15.9445 12.2325 15.4922 12.2325C15.0398 12.2325 14.6719 12.5977 14.6719 13.0465C14.6719 13.4953 15.0398 13.8604 15.4922 13.8604C15.5808 13.8604 15.6662 13.8464 15.7462 13.8205C15.756 13.8169 15.7658 13.8135 15.7757 13.8103ZM9.14768 17.7093L11.3213 16.9099C11.2137 17.2508 11.1586 17.6079 11.1586 17.972V20.6046H7.7344V19.7232C7.7344 18.8302 8.30159 18.0209 9.14768 17.7093ZM10.179 14.8372C10.179 14.4535 10.4954 14.1395 10.8821 14.1395C11.2688 14.1395 11.5852 14.4535 11.5852 14.8372C11.5852 15.2209 11.2688 15.5349 10.8821 15.5349C10.4954 15.5349 10.179 15.2209 10.179 14.8372ZM19.4039 11.2069C19.4039 10.7185 19.8047 10.3208 20.2969 10.3208C20.7891 10.3208 21.1899 10.7185 21.1899 11.2069C21.1899 11.6952 20.7891 12.0929 20.2969 12.0929C19.8047 12.0929 19.4039 11.6952 19.4039 11.2069Z" />
        </svg>
      );
    default:
      return null;
  }
};

export const OAuthConnect: React.FC<OAuthConnectProps> = ({
  provider,
  onConnected,
  disabled = false,
  initialLocalPath = '',
  onLocalPathChange,
  saveConnection = false,
  onSaveConnectionChange,
  connectionName = '',
  onConnectionNameChange,
}) => {
  const { t } = useI18n();
  const { isAuthenticating, error, startAuth, connect, hasTokens, logout } = useOAuth2();
  const [hasExistingTokens, setHasExistingTokens] = useState(false);
  const [showCredentialsForm, setShowCredentialsForm] = useState(false);
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [isChecking, setIsChecking] = useState(true);
  const [localPath, setLocalPath] = useState(initialLocalPath);

  // Sync local path when parent updates (e.g. switching between saved servers to edit)
  useEffect(() => {
    setLocalPath(initialLocalPath);
  }, [initialLocalPath]);

  const [wantToSave, setWantToSave] = useState(saveConnection);
  const [saveName, setSaveName] = useState(connectionName);
  const [isLoggingOut, setIsLoggingOut] = useState(false);
  const [wantsNewAccount, setWantsNewAccount] = useState(false);
  const [showSecret, setShowSecret] = useState(false);
  const [zohoRegion, setZohoRegion] = useState('us');

  const isZoho = provider === 'zohoworkdrive';
  const oauthProvider = providerMap[provider];
  const oauthApp = OAUTH_APPS[oauthProvider];

  // Browse for local folder
  const browseLocalFolder = async () => {
    try {
      const selected = await open({ directory: true, multiple: false, title: t('connection.oauth.selectLocalFolder') });
      if (selected && typeof selected === 'string') {
        setLocalPath(selected);
        onLocalPathChange?.(selected);
      }
    } catch (e) {
      console.error('Folder picker error:', e);
    }
  };

  // Check for existing tokens on mount
  useEffect(() => {
    const checkTokens = async () => {
      setIsChecking(true);
      const exists = await hasTokens(oauthProvider);
      setHasExistingTokens(exists);
      setIsChecking(false);
    };
    checkTokens();
  }, [oauthProvider, hasTokens]);

  // Load saved credentials from secure credential store (fallback: localStorage for migration)
  // Reset credentials first when provider changes to avoid showing stale values
  useEffect(() => {
    // Reset to empty before loading new provider's credentials
    setClientId('');
    setClientSecret('');
    setShowCredentialsForm(false);

    const loadCredentials = async () => {
      try {
        const savedId = await invoke<string>('get_credential', { account: `oauth_${provider}_client_id` });
        if (savedId) setClientId(savedId);
      } catch {
        // SEC: No localStorage fallback — credentials must be in vault.
      }
      try {
        const savedSecret = await invoke<string>('get_credential', { account: `oauth_${provider}_client_secret` });
        if (savedSecret) setClientSecret(savedSecret);
      } catch {
        // SEC: No localStorage fallback — credentials must be in vault.
      }
      // Load saved Zoho region
      if (isZoho) {
        try {
          const savedRegion = await invoke<string>('get_credential', { account: `oauth_${provider}_region` });
          if (savedRegion) setZohoRegion(savedRegion);
        } catch {
          // Default 'us' already set
        }
      }
    };
    loadCredentials();
  }, [provider, isZoho]);

  const handleSignIn = async () => {
    if (!clientId || !clientSecret) {
      setShowCredentialsForm(true);
      return;
    }

    // Save credentials to secure credential store
    invoke('store_credential', { account: `oauth_${provider}_client_id`, password: clientId }).catch(console.error);
    invoke('store_credential', { account: `oauth_${provider}_client_secret`, password: clientSecret }).catch(console.error);
    // Save Zoho region to credential store
    if (isZoho) {
      invoke('store_credential', { account: `oauth_${provider}_region`, password: zohoRegion }).catch(console.error);
    }
    // Remove legacy localStorage entries
    localStorage.removeItem(`oauth_${provider}_client_id`);
    localStorage.removeItem(`oauth_${provider}_client_secret`);

    try {
      const params = {
        provider: oauthProvider,
        client_id: clientId,
        client_secret: clientSecret,
        ...(isZoho && { region: zohoRegion }),
      };

      // Start OAuth flow (opens browser)
      await startAuth(params);

      // For now, we need to wait for the callback
      // In a real implementation, we'd use deep linking or a callback server
      // The callback server in Rust handles this automatically

      // After successful auth, connect to the provider
      const displayName = await connect(params);
      onConnected(displayName, isZoho ? { region: zohoRegion } : undefined);
    } catch (e) {
      console.error('OAuth error:', e);
    }
  };

  const handleQuickConnect = async () => {
    if (!clientId || !clientSecret) {
      setShowCredentialsForm(true);
      return;
    }

    logger.debug('[OAuthConnect] handleQuickConnect called for', oauthProvider);
    logger.debug('[OAuthConnect] clientId:', clientId?.slice(0, 20) + '...');

    try {
      const params = {
        provider: oauthProvider,
        client_id: clientId,
        client_secret: clientSecret,
        ...(isZoho && { region: zohoRegion }),
      };
      logger.debug('[OAuthConnect] Calling oauth2_connect...');
      const displayName = await connect(params);
      logger.debug('[OAuthConnect] Connected, displayName:', displayName);
      onConnected(displayName, isZoho ? { region: zohoRegion } : undefined);
    } catch (e) {
      console.error('[OAuthConnect] Quick connect error:', e);
    }
  };

  const handleLogout = async () => {
    setIsLoggingOut(true);
    try {
      await logout(oauthProvider);
      setHasExistingTokens(false);
      setWantsNewAccount(false);
    } catch (e) {
      console.error('Logout error:', e);
    } finally {
      setIsLoggingOut(false);
    }
  };

  const handleUseNewAccount = () => {
    setWantsNewAccount(true);
  };

  if (isChecking) {
    return (
      <div className="flex items-center justify-center p-4">
        <Loader2 className="w-5 h-5 animate-spin text-gray-400" />
        <span className="ml-2 text-sm text-gray-500">{t('common.loading')}</span>
      </div>
    );
  }

  // Show "Active" state when already authenticated (like AeroCloud)
  if (hasExistingTokens && !wantsNewAccount) {
    return (
      <div className="space-y-4">
        {/* Active Status Card */}
        <div className={`p-4 rounded-xl border-2 ${provider === 'googledrive' ? 'border-red-500/30 bg-red-500/5' :
          provider === 'dropbox' ? 'border-blue-500/30 bg-blue-500/5' :
            'border-sky-500/30 bg-sky-500/5'
          }`}>
          <div className="flex items-center gap-3">
            <div className={`w-12 h-12 rounded-xl flex items-center justify-center ${provider === 'googledrive' ? 'bg-red-500/20' :
              provider === 'dropbox' ? 'bg-blue-500/20' :
                'bg-sky-500/20'
              }`}>
              <ProviderIcon provider={provider} className="w-6 h-6" />
            </div>
            <div className="flex-1">
              <div className="flex items-center gap-2">
                <span className="font-medium">{providerNames[provider]}</span>
                <span className="px-2 py-0.5 text-xs font-medium bg-green-500/20 text-green-400 rounded-full flex items-center gap-1">
                  <CheckCircle size={12} />
                  {t('connection.active')}
                </span>
              </div>
              <span className="text-sm text-gray-500">{t('connection.oauth.previouslyAuthenticated')}</span>
            </div>
          </div>
        </div>

        {/* Local Path (optional — editable even in quick-connect mode) */}
        <div>
          <label className="block text-sm font-medium mb-1.5">{t('connection.oauth.localFolderOptional')}</label>
          <div className="flex gap-2">
            <input
              type="text"
              value={localPath}
              onChange={(e) => {
                setLocalPath(e.target.value);
                onLocalPathChange?.(e.target.value);
              }}
              placeholder="~/Downloads"
              className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-sm"
            />
            <button
              type="button"
              onClick={browseLocalFolder}
              className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-xl"
              title={t('common.browse')}
            >
              <FolderOpen size={18} />
            </button>
          </div>
        </div>

        {/* Quick Connect Button */}
        <button
          onClick={handleQuickConnect}
          disabled={disabled || isAuthenticating}
          className={`
            w-full py-3 px-4 rounded-xl text-white font-medium
            flex items-center justify-center gap-2 transition-colors
            ${providerColors[provider]}
            disabled:opacity-50 disabled:cursor-not-allowed
          `}
        >
          {isAuthenticating ? (
            <>
              <Loader2 className="w-5 h-5 animate-spin" />
              {t('connection.connecting')}
            </>
          ) : (
            <>
              <ProviderIcon provider={provider} className="w-5 h-5" white />
              {t('connection.oauth.connectTo', { provider: providerNames[provider] })}
            </>
          )}
        </button>

        {/* Use Different Account */}
        <div className="flex gap-2">
          <button
            onClick={handleUseNewAccount}
            className="flex-1 py-2 px-3 text-sm text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-xl flex items-center justify-center gap-2 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
          >
            <RefreshCw size={14} />
            {t('connection.oauth.useDifferentAccount')}
          </button>
          <button
            onClick={handleLogout}
            disabled={isLoggingOut}
            className="py-2 px-3 text-sm text-red-500 hover:text-red-600 border border-red-300 dark:border-red-600/50 rounded-xl flex items-center justify-center gap-2 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors disabled:opacity-50"
            title={t('connection.oauth.disconnectAccount')}
          >
            {isLoggingOut ? <Loader2 size={14} className="animate-spin" /> : <LogOut size={14} />}
          </button>
        </div>

        {/* Error Display */}
        {error && (
          <div className="p-3 bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded-lg">
            <div className="flex items-start gap-2 text-red-700 dark:text-red-300">
              <AlertCircle className="w-5 h-5 flex-shrink-0 mt-0.5" />
              <span className="text-sm">{error}</span>
            </div>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="space-y-4">
      {/* Local Path (optional) */}
      <div>
        <label className="block text-sm font-medium mb-1.5">{t('connection.oauth.localFolderOptional')}</label>
        <div className="flex gap-2">
          <input
            type="text"
            value={localPath}
            onChange={(e) => {
              setLocalPath(e.target.value);
              onLocalPathChange?.(e.target.value);
            }}
            placeholder="~/Downloads"
            className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-sm"
          />
          <button
            type="button"
            onClick={browseLocalFolder}
            className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-xl"
            title={t('common.browse')}
          >
            <FolderOpen size={18} />
          </button>
        </div>
      </div>

      {/* Save Connection Option */}
      <div className="flex items-center gap-3 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-xl">
        <input
          type="checkbox"
          id={`save-oauth-${provider}`}
          checked={wantToSave}
          onChange={(e) => {
            setWantToSave(e.target.checked);
            onSaveConnectionChange?.(e.target.checked);
          }}
          className="w-4 h-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
        />
        <label htmlFor={`save-oauth-${provider}`} className="flex-1">
          <span className="text-sm font-medium">{t('connection.saveThisConnection')}</span>
          <p className="text-xs text-gray-500">{t('connection.oauth.quickConnectNextTime')}</p>
        </label>
        <Save size={16} className="text-gray-400" />
      </div>

      {/* Connection Name (if saving) */}
      {wantToSave && (
        <div>
          <label className="block text-sm font-medium mb-1.5">{t('connection.connectionNameOptional')}</label>
          <input
            type="text"
            value={saveName}
            onChange={(e) => {
              setSaveName(e.target.value);
              onConnectionNameChange?.(e.target.value);
            }}
            placeholder={t('connection.oauth.myProvider', { provider: providerNames[provider] })}
            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-sm"
          />
        </div>
      )}

      {/* Provider Sign In Button */}
      <div className="text-center">
        <button
          onClick={hasExistingTokens ? handleQuickConnect : handleSignIn}
          disabled={disabled || isAuthenticating}
          className={`
            w-full py-3 px-4 rounded-lg text-white font-medium
            flex items-center justify-center gap-2 transition-colors
            ${providerColors[provider]}
            disabled:opacity-50 disabled:cursor-not-allowed
          `}
        >
          {isAuthenticating ? (
            <>
              <Loader2 className="w-5 h-5 animate-spin" />
              {t('connection.authenticating')}
            </>
          ) : hasExistingTokens ? (
            <>
              <ProviderIcon provider={provider} className="w-5 h-5" white />
              {t('connection.oauth.connectTo', { provider: providerNames[provider] })}
            </>
          ) : (
            <>
              <ProviderIcon provider={provider} className="w-5 h-5" white />
              {t('connection.oauth.signInWith', { provider: providerNames[provider] })}
            </>
          )}
        </button>
      </div>

      {/* Error Display */}
      {error && (
        <div className="p-3 bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded-lg">
          <div className="flex items-start gap-2 text-red-700 dark:text-red-300">
            <AlertCircle className="w-5 h-5 flex-shrink-0 mt-0.5" />
            <span className="text-sm">{error}</span>
          </div>
        </div>
      )}

      {/* Credentials Form */}
      {showCredentialsForm && (
        <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
          <div className="flex items-center justify-between">
            <h4 className="font-medium text-sm">{t('connection.oauth.oauth2Credentials')}</h4>
            <button
              onClick={() => openUrl(oauthApp.help_url)}
              className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
            >
              {t('settings.getCredentials')} <ExternalLink className="w-3 h-3" />
            </button>
          </div>

          <p className="text-xs text-gray-500 dark:text-gray-400">
            {t('connection.oauth.createAppInstructions', { provider: providerNames[provider] })}
          </p>

          {/* Zoho Region selector */}
          {isZoho && (
            <div>
              <label className="block text-xs font-medium mb-1">{t('connection.oauth.zohoRegion')}</label>
              <select
                value={zohoRegion}
                onChange={(e) => setZohoRegion(e.target.value)}
                className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
              >
                {ZOHO_REGIONS.map(r => (
                  <option key={r.value} value={r.value}>{r.label}</option>
                ))}
              </select>
              <p className="text-xs text-gray-500 mt-1">{t('connection.oauth.zohoRegionHelp')}</p>
            </div>
          )}

          <div>
            <label className="block text-xs font-medium mb-1">{t('settings.clientId')}</label>
            <input
              type="text"
              value={clientId}
              onChange={(e) => setClientId(e.target.value)}
              placeholder={t('connection.oauth.enterClientId')}
              className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
            />
          </div>

          <div>
            <label className="block text-xs font-medium mb-1">{t('settings.clientSecret')}</label>
            <div className="relative">
              <input
                type={showSecret ? 'text' : 'password'}
                value={clientSecret}
                onChange={(e) => setClientSecret(e.target.value)}
                placeholder={t('connection.oauth.enterClientSecret')}
                className="w-full px-3 py-2 pr-10 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
              />
              <button type="button" onClick={() => setShowSecret(!showSecret)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                {showSecret ? <EyeOff size={16} /> : <Eye size={16} />}
              </button>
            </div>
          </div>

          <div className="flex gap-2">
            <button
              onClick={() => setShowCredentialsForm(false)}
              className="flex-1 py-2 px-3 text-sm border rounded-lg hover:bg-gray-100 dark:hover:bg-gray-600"
            >
              {t('common.cancel')}
            </button>
            <button
              onClick={handleSignIn}
              disabled={!clientId || !clientSecret}
              className={`flex-1 py-2 px-3 text-sm text-white rounded-lg ${providerColors[provider]} disabled:opacity-50`}
            >
              {t('connection.oauth.continue')}
            </button>
          </div>
        </div>
      )}

      {/* Setup instructions toggle */}
      {!showCredentialsForm && (
        <button
          onClick={() => setShowCredentialsForm(true)}
          className="w-full py-2 text-sm text-gray-500 hover:text-gray-700 dark:hover:text-gray-300 flex items-center justify-center gap-1"
        >
          <Settings className="w-4 h-4" />
          {t('connection.oauth.configureCredentials')}
        </button>
      )}

      {/* Back to existing account (if user changed mind) */}
      {wantsNewAccount && hasExistingTokens && (
        <button
          onClick={() => setWantsNewAccount(false)}
          className="w-full py-2 text-sm text-blue-500 hover:text-blue-600 flex items-center justify-center gap-1"
        >
          ← {t('connection.oauth.backToExistingAccount')}
        </button>
      )}
    </div>
  );
};

export default OAuthConnect;