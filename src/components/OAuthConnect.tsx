// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** OAuth2 Quick Connect: credentials, exact callback URI, and sign-in. */
import React, { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { AlertCircle, Check, Copy, ExternalLink, Eye, EyeOff, Loader2, Save } from 'lucide-react';
import { Checkbox } from './ui/Checkbox';
import { useOAuth2, type OAuthProvider, OAUTH_APPS } from '../hooks/useOAuth2';
import { useI18n } from '../i18n';
import { openUrl } from '../utils/openUrl';
import { CopyLinkButton } from './common/CopyLinkButton';
import { useClipboardCopy } from '../hooks/useClipboardCopy';
import {
  BoxLogo,
  DropboxLogo,
  GoogleDriveLogo,
  GooglePhotosLogo,
  OneDriveLogo,
  PCloudLogo,
  YandexDiskLogo,
  ZohoWorkDriveLogo,
} from './ProviderLogos';

type OAuthUiProvider = 'googledrive' | 'googlephotos' | 'dropbox' | 'onedrive' | 'box' | 'pcloud' | 'zohoworkdrive' | 'yandexdisk';

interface OAuthConnectProps {
  provider: OAuthUiProvider;
  onConnected: (displayName: string, extraOptions?: { region?: string }) => void;
  disabled?: boolean;
  initialLocalPath?: string;
  onLocalPathChange?: (path: string) => void;
  saveConnection?: boolean;
  onSaveConnectionChange?: (save: boolean) => void;
  connectionName?: string;
  onConnectionNameChange?: (name: string) => void;
  isEditing?: boolean;
  existingNames?: string[];
  rightColumn?: React.ReactNode;
}

const providerMap: Record<OAuthUiProvider, OAuthProvider> = {
  googledrive: 'google_drive',
  googlephotos: 'googlephotos',
  dropbox: 'dropbox',
  onedrive: 'onedrive',
  box: 'box',
  pcloud: 'pcloud',
  zohoworkdrive: 'zoho_workdrive',
  yandexdisk: 'yandexdisk',
};

const credentialAlias: Partial<Record<OAuthUiProvider, OAuthUiProvider>> = {
  googlephotos: 'googledrive',
};

const providerNames: Record<OAuthUiProvider, string> = {
  googledrive: 'Google Drive',
  googlephotos: 'Google Photos',
  dropbox: 'Dropbox',
  onedrive: 'OneDrive',
  box: 'Box',
  pcloud: 'pCloud Drive',
  zohoworkdrive: 'Zoho WorkDrive',
  yandexdisk: 'Yandex Disk',
};

const providerColors: Record<OAuthUiProvider, string> = {
  googledrive: 'bg-white text-[#1F1F1F] border border-[#747775] hover:bg-[#f2f2f2] dark:bg-[#131314] dark:text-[#E3E3E3] dark:border-[#8E918F] dark:hover:bg-[#1e1f20]',
  googlephotos: 'text-white bg-amber-500 hover:bg-amber-600',
  dropbox: 'text-white bg-blue-500 hover:bg-blue-600',
  onedrive: 'text-white bg-sky-500 hover:bg-sky-600',
  box: 'text-white bg-blue-600 hover:bg-blue-700',
  pcloud: 'text-white bg-green-500 hover:bg-green-600',
  zohoworkdrive: 'text-white bg-blue-700 hover:bg-blue-800',
  yandexdisk: 'text-white bg-yellow-500 hover:bg-yellow-600',
};

const providerLogos: Record<OAuthUiProvider, React.FC<{ size?: number }>> = {
  googledrive: GoogleDriveLogo,
  googlephotos: GooglePhotosLogo,
  dropbox: DropboxLogo,
  onedrive: OneDriveLogo,
  box: BoxLogo,
  pcloud: PCloudLogo,
  zohoworkdrive: ZohoWorkDriveLogo,
  yandexdisk: YandexDiskLogo,
};

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

export const OAuthConnect: React.FC<OAuthConnectProps> = ({
  provider,
  onConnected,
  disabled = false,
  saveConnection = false,
  onSaveConnectionChange,
  isEditing = false,
  rightColumn,
}) => {
  const { t } = useI18n();
  const { isAuthenticating, error, startAuth, connect } = useOAuth2();
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [showSecret, setShowSecret] = useState(false);
  const [zohoRegion, setZohoRegion] = useState('us');
  const [wantToSave, setWantToSave] = useState(saveConnection);
  const [redirectUri, setRedirectUri] = useState('');
  const { copied: copiedUri, copy: copyRedirectUri } = useClipboardCopy();

  const oauthProvider = providerMap[provider];
  const credentialProvider = credentialAlias[provider] || provider;
  const oauthApp = OAUTH_APPS[providerMap[credentialProvider] as keyof typeof OAUTH_APPS];
  const isZoho = provider === 'zohoworkdrive';
  const ProviderLogo = providerLogos[provider];

  useEffect(() => setWantToSave(saveConnection), [saveConnection]);

  useEffect(() => {
    if (isEditing && !wantToSave) {
      setWantToSave(true);
      onSaveConnectionChange?.(true);
    }
    // The edit transition is the only relevant event; callbacks are intentionally
    // omitted so an unstable parent closure cannot retrigger the state update.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isEditing]);

  useEffect(() => {
    setClientId('');
    setClientSecret('');
    const load = async () => {
      try {
        setClientId(await invoke<string>('get_credential', { account: `oauth_${credentialProvider}_client_id` }));
      } catch { /* No stored credential. */ }
      try {
        setClientSecret(await invoke<string>('get_credential', { account: `oauth_${credentialProvider}_client_secret` }));
      } catch { /* No stored credential. */ }
      if (isZoho) {
        try {
          setZohoRegion(await invoke<string>('get_credential', { account: `oauth_${provider}_region` }));
        } catch { /* Keep US default. */ }
      }
    };
    void load();
  }, [credentialProvider, isZoho, provider]);

  // The backend owns listener host, port and callback path. Displaying that
  // exact value prevents dashboard configuration from drifting from runtime.
  useEffect(() => {
    setRedirectUri('');
    invoke<string>('oauth2_redirect_uri', { provider: oauthProvider })
      .then(setRedirectUri)
      .catch((reason) => console.error('Could not resolve OAuth redirect URI', reason));
  }, [oauthProvider]);

  const handleSignIn = async () => {
    if (!clientId || !clientSecret) return;
    await Promise.all([
      invoke('store_credential', { account: `oauth_${credentialProvider}_client_id`, password: clientId }),
      invoke('store_credential', { account: `oauth_${credentialProvider}_client_secret`, password: clientSecret }),
      ...(isZoho
        ? [invoke('store_credential', { account: `oauth_${provider}_region`, password: zohoRegion })]
        : []),
    ]);
    localStorage.removeItem(`oauth_${provider}_client_id`);
    localStorage.removeItem(`oauth_${provider}_client_secret`);

    const params = {
      provider: oauthProvider,
      client_id: clientId,
      client_secret: clientSecret,
      ...(isZoho && { region: zohoRegion }),
    };
    try {
      await startAuth(params);
      const displayName = await connect(params);
      onConnected(displayName, isZoho ? { region: zohoRegion } : undefined);
    } catch (reason) {
      console.error('OAuth error:', reason);
    }
  };

  const saveToggle = !isEditing && (
    <div className="flex items-center gap-3 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
      <Checkbox
        checked={wantToSave}
        onChange={(value) => {
          setWantToSave(value);
          onSaveConnectionChange?.(value);
        }}
        label={<div className="flex-1">
          <span className="text-sm font-medium">{t('connection.saveThisConnection')}</span>
          <p className="text-xs text-gray-500">{t('connection.oauth.quickConnectNextTime')}</p>
        </div>}
      />
      <Save size={16} className="text-gray-400" />
    </div>
  );

  return (
    <div className="grid md:grid-cols-2 gap-6 items-start">
      <div className="space-y-4 min-w-0">
        {error && (
          <div className="p-3 bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded-lg">
            <div className="flex items-start gap-2 text-red-700 dark:text-red-300">
              <AlertCircle className="w-5 h-5 flex-shrink-0 mt-0.5" />
              <span className="text-sm">{error}</span>
            </div>
          </div>
        )}

        <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
          <div className="flex items-center justify-between gap-3">
            <h4 className="font-medium text-sm">{t('connection.oauth.oauth2Credentials')}</h4>
            <div className="flex items-center shrink-0">
              <button type="button" onClick={() => openUrl(oauthApp.help_url)} className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1">
                {t(provider === 'pcloud' ? 'settings.manageCredentials' : 'settings.getCredentials')}
                <ExternalLink className="w-3 h-3" />
              </button>
              <CopyLinkButton url={oauthApp.help_url} size={12} />
            </div>
          </div>

          <p className="text-xs text-gray-500 dark:text-gray-400">
            {t('connection.oauth.createAppInstructions', { provider: providerNames[provider] })}
          </p>

          {redirectUri && (
            <div>
              <label className="block text-xs font-medium mb-1">{t('connection.oauth.redirectUri')}</label>
              <div className="flex items-center gap-1.5">
                <code className="flex-1 px-3 py-2 text-xs font-mono bg-gray-100 dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg select-all truncate">
                  {redirectUri}
                </code>
                <button type="button" onClick={() => void copyRedirectUri(redirectUri)} className="shrink-0 p-2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 rounded-lg" title={t('common.copy')}>
                  {copiedUri ? <Check size={14} className="text-green-500" /> : <Copy size={14} />}
                </button>
              </div>
              <p className="text-xs text-gray-500 mt-1">{t('connection.oauth.redirectUriHelp')}</p>
            </div>
          )}

          {isZoho && (
            <div>
              <label className="block text-xs font-medium mb-1">{t('connection.oauth.zohoRegion')}</label>
              <select value={zohoRegion} onChange={(event) => setZohoRegion(event.target.value)} className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600">
                {ZOHO_REGIONS.map((region) => <option key={region.value} value={region.value}>{region.label}</option>)}
              </select>
              <p className="text-xs text-gray-500 mt-1">{t('connection.oauth.zohoRegionHelp')}</p>
            </div>
          )}

          <div>
            <label className="block text-xs font-medium mb-1">{t('settings.clientId')}</label>
            <input type="text" value={clientId} onChange={(event) => setClientId(event.target.value)} placeholder={t('connection.oauth.enterClientId')} className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600" />
          </div>

          <div>
            <label className="block text-xs font-medium mb-1">{t('settings.clientSecret')}</label>
            <div className="relative">
              <input type={showSecret ? 'text' : 'password'} value={clientSecret} onChange={(event) => setClientSecret(event.target.value)} placeholder={t('connection.oauth.enterClientSecret')} className="w-full px-3 py-2 pr-10 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600" />
              <button tabIndex={-1} type="button" onClick={() => setShowSecret(!showSecret)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                {showSecret ? <EyeOff size={16} /> : <Eye size={16} />}
              </button>
            </div>
          </div>

          <button type="button" onClick={() => void handleSignIn()} disabled={disabled || !clientId || !clientSecret || isAuthenticating} className={`w-full py-3 px-4 text-sm font-medium rounded-lg flex items-center justify-center gap-2 ${providerColors[provider]} disabled:opacity-50`}>
            {isAuthenticating
              ? <><Loader2 className="w-5 h-5 animate-spin" />{t('connection.authenticating')}</>
              : <><ProviderLogo size={20} />{t('connection.oauth.signInWith', { provider: providerNames[provider] })}</>}
          </button>

          {(provider === 'googledrive' || provider === 'googlephotos') && (
            <p className="text-xs text-gray-400 dark:text-gray-500 text-center mt-2">
              {t(provider === 'googlephotos' ? 'connection.oauth.enablePhotosApi' : 'connection.oauth.enableDriveApi')}
            </p>
          )}
        </div>
        {saveToggle}
      </div>
      {rightColumn}
    </div>
  );
};

export default OAuthConnect;
