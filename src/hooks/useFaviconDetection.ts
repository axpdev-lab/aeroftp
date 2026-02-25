import { useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { FtpSession, ServerProfile } from '../types';
import { secureGetWithFallback } from '../utils/secureStorage';

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

  useEffect(() => {
    if (!activeSessionId) return;

    const session = sessionsRef.current.find(s => s.id === activeSessionId);
    if (!session || session.status !== 'connected') return;

    const protocol = session.connectionParams.protocol || 'ftp';
    const isServerProtocol = SERVER_PROTOCOLS.has(protocol);
    const isProviderProtocol = PROVIDER_PROTOCOLS.has(protocol);
    if (!isServerProtocol && !isProviderProtocol) return;

    const serverKey = session.serverId;
    if (checkedRef.current.has(serverKey)) return;
    if (session.faviconUrl) {
      checkedRef.current.add(serverKey);
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
              s.id === serverKey || s.name === serverKey || s.host === serverKey
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

        const result = await invoke<string | null>(command, { searchPaths });

        if (cancelled) return;
        checkedRef.current.add(serverKey);

        if (result) {
          callbackRef.current(serverKey, result);
        }
      } catch {
        // Detection failed silently — server has no favicon
        if (!cancelled) {
          checkedRef.current.add(serverKey);
        }
      }
    };

    const timer = setTimeout(detect, 2000);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [activeSessionId]);
}
