// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useOAuth2 Hook
 * Manages OAuth2 authentication flows for cloud providers
 */

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { countedOAuthLogin, oauthLoginKey } from '../utils/oauthLogins';
import { translate } from '../i18n';

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

/** What a sign-in in the connection form tells App about the invoke that
 *  replaces the backend's provider: `start` before it, `settled` when it ends,
 *  either way. */
export interface BackendSwapDetail {
  token: number;
  phase: 'start' | 'settled';
}

let lastBackendSwapToken = 0;
let lastSettledBackendSwapToken = 0;

/** The token of the sign-in swap that settled last, the one whose provider
 *  the backend holds. The form reads it when its `onConnected` starts, which
 *  runs in the same task as that settle (no IPC reply can come between), so
 *  its report (`aeroftp-sign-in-handoff`, `aeroftp-sign-in-abandoned`) names
 *  its own sign-in even when another one is still running. */
export const latestSettledBackendSwapToken = (): number => lastSettledBackendSwapToken;

/** How long a sign-in waits for the one before it to settle, as long as a
 *  connect may take (`CONNECT_HARD_TIMEOUT_MS` in App). */
export const BACKEND_SWAP_WAIT_MS = 120_000;

/** Settles when the latest queued swap settles (or gives up waiting). Module
 *  state, so a window reload starts with an empty queue. */
let swapQueueTail: Promise<void> = Promise.resolve();

/** Runs a sign-in's invoke that replaces the backend's provider
 *  (`oauth2_connect`, `fourshared_connect`). App is told first, in the same
 *  task, so the live tab is put aside before the backend leaves it: the swap
 *  used to happen first, and until the form handed the connect over the tab
 *  stayed live, and the transfer queue could start on it, over the new
 *  account.
 *
 *  One swap at a time: a second sign-in is put aside and counted at once,
 *  but its invoke starts only once the one before it settled, so the backend
 *  installs the providers in the order their replies arrive and the swap that
 *  settled last is the one it holds. The live tab stays put aside for the
 *  whole wait (App counts the swap from its start). The wait is bounded: past
 *  `BACKEND_SWAP_WAIT_MS` the sign-in fails with the timed-out connect text,
 *  replacing nothing and owning nothing, and its place in the queue passes on
 *  the swap still running before it, so the sign-ins behind it keep waiting
 *  for that one, each with its own bounded wait. A sign-in whose invoke ran,
 *  even one that threw at once or was rejected, frees its place when it
 *  settles. */
export function swapBackend<T>(run: () => Promise<T>): Promise<T> {
  const token = ++lastBackendSwapToken;
  let ran = false;
  const tell = (phase: BackendSwapDetail['phase']) => {
    if (phase === 'settled' && ran) lastSettledBackendSwapToken = token;
    window.dispatchEvent(new CustomEvent<BackendSwapDetail>('aeroftp-backend-swap', { detail: { token, phase } }));
  };
  tell('start');
  const previous = swapQueueTail;
  let release!: () => void;
  swapQueueTail = new Promise<void>((resolve) => { release = resolve; });
  let timer: ReturnType<typeof setTimeout> | undefined;
  const turn = new Promise<boolean>((resolve) => {
    timer = setTimeout(() => resolve(false), BACKEND_SWAP_WAIT_MS);
    void previous.then(() => resolve(true));
  });
  let handedOn = false;
  return turn
    .then((ready) => {
      clearTimeout(timer);
      if (!ready) {
        handedOn = true;
        void previous.then(release);
        throw new Error(translate('toast.connectTimedOut'));
      }
      ran = true;
      return run();
    })
    .finally(() => {
      if (!handedOn) release();
      tell('settled');
    });
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
      const result = await countedOAuthLogin(oauthLoginKey(params.provider, params.profile_id), () => invoke<string>('oauth2_full_auth', { params }));
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
      const result = await swapBackend(() => invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params }));
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
