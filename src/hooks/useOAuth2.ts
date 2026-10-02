// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useOAuth2 Hook
 * Manages OAuth2 authentication flows for cloud providers
 */

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';

export type OAuthProvider = 'google_drive' | 'googlephotos' | 'dropbox' | 'onedrive' | 'box' | 'pcloud' | 'zoho_workdrive' | 'yandexdisk';

interface OAuthFlowStarted {
  auth_url: string;
  state: string;
}

interface OAuthConnectionParams {
  provider: OAuthProvider;
  client_id: string;
  client_secret: string;
  region?: string;
  /**
   * Server profile identifier that owns the tokens. When supplied the vault
   * scopes the OAuth blob to `oauth_<provider>_<profile_id>`, so two profiles
   * pointing at distinct accounts of the same provider can coexist on the
   * same device. Omitted requests keep the legacy singleton key. Issue #214.
   */
  profile_id?: string;
}

interface UseOAuth2Return {
  isAuthenticating: boolean;
  error: string | null;
  startAuth: (params: OAuthConnectionParams) => Promise<OAuthFlowStarted>;
  connect: (params: OAuthConnectionParams) => Promise<string>;
}

/**
 * Custom hook for OAuth2 authentication with cloud providers
 */
export function useOAuth2(): UseOAuth2Return {
  const [isAuthenticating, setIsAuthenticating] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Run the whole OAuth2 flow through `oauth2_full_auth`: the backend opens the
   * browser and waits for the loopback callback itself.
   */
  const startAuth = useCallback(async (params: OAuthConnectionParams): Promise<OAuthFlowStarted> => {
    setIsAuthenticating(true);
    setError(null);
    
    try {
      // Use the new full auth flow that handles everything automatically
      const result = await invoke<string>('oauth2_full_auth', { params });
      // Return a mock result since full_auth completes the flow
      return { auth_url: '', state: result };
    } catch (e) {
      const errorMsg = e instanceof Error ? e.message : String(e);
      setError(errorMsg);
      setIsAuthenticating(false);
      throw e;
    }
  }, []);

  /**
   * Connect to OAuth2 provider after authentication
   */
  const connect = useCallback(async (params: OAuthConnectionParams): Promise<string> => {
    try {
      const result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params });
      setIsAuthenticating(false);
      return result.display_name;
    } catch (e) {
      const errorMsg = e instanceof Error ? e.message : String(e);
      setError(errorMsg);
      setIsAuthenticating(false);
      throw e;
    }
  }, []);

  return {
    isAuthenticating,
    error,
    startAuth,
    connect,
  };
}

// OAuth2 client credentials (these should ideally come from environment/config)
// Users need to register their own apps with each provider
export const OAUTH_APPS = {
  google_drive: {
    // Placeholder - users need to set up their own Google Cloud Console app
    client_id: '',
    client_secret: '',
    help_url: 'https://console.cloud.google.com/apis/credentials',
  },
  dropbox: {
    // Placeholder - users need to set up their own Dropbox App
    client_id: '',
    client_secret: '',
    help_url: 'https://www.dropbox.com/developers/apps',
  },
  onedrive: {
    // Placeholder - users need to set up their own Azure AD app
    client_id: '',
    client_secret: '',
    // App Registrations list blade: the deep link that lands directly on the
    // page where the user creates the app for the OneDrive Client ID / Secret
    // (#270). The older #blade/ form no longer resolves reliably.
    help_url: 'https://portal.azure.com/#view/Microsoft_AAD_RegisteredApps/ApplicationsListBlade',
  },
  box: {
    // Placeholder - users need to set up their own Box Developer app
    client_id: '',
    client_secret: '',
    help_url: 'https://app.box.com/developers/console',
  },
  pcloud: {
    // Placeholder - users need to set up their own pCloud app
    client_id: '',
    client_secret: '',
    // #369: the OAuth authorize reference overwhelms and cannot create an app;
    // point at the app-management page (the trailing slash matters, the bare
    // path redirects to the docs root).
    help_url: 'https://docs.pcloud.com/my_apps/',
  },
  zoho_workdrive: {
    // Users need to create a Server-based Application at api-console.zoho.com
    client_id: '',
    client_secret: '',
    help_url: 'https://api-console.zoho.com/',
  },
  yandexdisk: {
    // Users need to create an OAuth app at oauth.yandex.com
    client_id: '',
    client_secret: '',
    help_url: 'https://oauth.yandex.com/client/new',
  },
};
