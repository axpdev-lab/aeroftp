// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { FtpSession, ServerProfile } from '../types';
import { secureGetWithFallback } from '../utils/secureStorage';
import { logger } from '../utils/logger';

// FTP/FTPS use ftp_manager (suppaftp) → detect_server_favicon
const SERVER_PROTOCOLS = new Set(['ftp', 'ftps']);
// SFTP/S3/WebDAV use StorageProvider (ProviderState) → detect_provider_favicon
const PROVIDER_PROTOCOLS = new Set(['sftp', 's3', 'webdav']);

/**
 * Hook that detects project favicons from connected FTP/FTPS/SFTP/S3/WebDAV servers.
 * Searches favicon.ico first, then manifest.json/site.webmanifest as fallback.
 * Uses initialPath (project web root) → current remote path → / as search paths.
 */
export function useFaviconDetection(
  sessions: FtpSession[],
  activeSessionId: string | null,
  onFaviconDetected: (serverId: string, faviconUrl: string) => void,
) {
  const checkedRef = useRef<Set<string>>(new Set());
  const sessionsRef = useRef(sessions);
  sessionsRef.current = sessions;
  const callbackRef = useRef(onFaviconDetected);
  callbackRef.current = onFaviconDetected;

  // Track status of the active session so the effect re-runs when it transitions
  // (e.g. connecting → connected). Previously the effect only depended on
  // activeSessionId, so if the id was set before the connection completed the
  // detection skipped silently and never retried.
  const activeSession = sessions.find(s => s.id === activeSessionId);
  const activeStatus = activeSession?.status;
  const activeHasFavicon = !!activeSession?.faviconUrl;

  useEffect(() => {
    if (!activeSessionId) return;

    const session = sessionsRef.current.find(s => s.id === activeSessionId);
    if (!session || session.status !== 'connected') return;

    const protocol = session.connectionParams.protocol || 'ftp';
    const isServerProtocol = SERVER_PROTOCOLS.has(protocol);
    const isProviderProtocol = PROVIDER_PROTOCOLS.has(protocol);
    if (!isServerProtocol && !isProviderProtocol) return;

    // Use session.id (unique per connection) so each new connection gets a fresh check.
    // Previously used serverId which cached negative results across connections.
    const cacheKey = session.id;
    if (checkedRef.current.has(cacheKey)) return;
    if (session.faviconUrl) {
      checkedRef.current.add(cacheKey);
      return;
    }

    let cancelled = false;

    const detect = async () => {
      try {
        // Build search paths: initialPath (project root) → current path → /
        const searchPaths: string[] = [];
        try {
          const servers = await secureGetWithFallback<ServerProfile[]>('server_profiles', 'aeroftp-saved-servers');
          if (servers) {
            const match = servers.find(s =>
              s.id === session.serverId || s.name === session.serverId || s.host === session.serverId
            );
            if (match?.initialPath) {
              searchPaths.push(match.initialPath);
            }
          }
        } catch { /* ignore */ }

        if (cancelled) return;

        if (session.remotePath && !searchPaths.includes(session.remotePath)) {
          searchPaths.push(session.remotePath);
        }
        if (!searchPaths.includes('/')) {
          searchPaths.push('/');
        }

        // FTP/FTPS → ftp_manager (suppaftp)
        // SFTP/S3/WebDAV → StorageProvider (ProviderState)
        const command = isProviderProtocol ? 'detect_provider_favicon' : 'detect_server_favicon';

        logger.debug('favicon-detection: invoking', { command, searchPaths, sessionId: session.id, protocol });

        const result = await invoke<string | null>(command, { searchPaths });

        if (cancelled) return;
        checkedRef.current.add(cacheKey);

        if (result) {
          logger.debug('favicon-detection: hit', { sessionId: session.id, length: result.length });
          callbackRef.current(session.serverId, result);
        } else {
          logger.debug('favicon-detection: miss', { sessionId: session.id, command, searchPaths });
        }
      } catch (e) {
        // Detection failed silently — log to dev console so first-run failures
        // surface to the user instead of being swallowed.
        logger.warn('favicon-detection: error', e);
        if (!cancelled) {
          checkedRef.current.add(cacheKey);
        }
      }
    };

    const timer = setTimeout(detect, 2000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [activeSessionId, activeStatus, activeHasFavicon]);
}
