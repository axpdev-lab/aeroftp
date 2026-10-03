// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { FileListResponse, LocalFile, RemoteFile } from '../types';
import type { FileBatchDirection } from './fileBatchRouting';

type DestinationInvoke = <T>(command: string, args?: Record<string, unknown>) => Promise<T>;
export interface OverwriteDestination {
  direction: FileBatchDirection;
  path: string;
  isProviderSession: boolean;
  aeroVaultSessionId?: string;
  visibleRemotePath: string;
}

/** Read the actual destination without navigating either panel or changing cwd.
 * A caller owns this snapshot for one transfer or batch; listing errors propagate
 * so an unknown directory can never be mistaken for an empty one.
 */
export async function loadOverwriteDestination(
  target: OverwriteDestination,
  invoke: DestinationInvoke,
): Promise<LocalFile[] | RemoteFile[]> {
  if (target.direction === 'download') {
    const files = await invoke<LocalFile[]>('get_local_files', { path: target.path, showHidden: true });
    if (!Array.isArray(files)) throw new Error('Invalid local destination listing');
    return files;
  }

  let response: FileListResponse;
  if (target.aeroVaultSessionId) {
    // aerovault_overlay_list(path) changes the overlay's current directory.
    // Its upload command also writes there, so another directory cannot be
    // inspected or targeted safely through this surface.
    if (target.path !== target.visibleRemotePath) {
      throw new Error('Cannot inspect another AeroVault overlay directory without navigation');
    }
    response = await invoke<FileListResponse>('aerovault_overlay_list', {
      sessionId: target.aeroVaultSessionId, path: null,
    });
  } else if (target.isProviderSession) {
    // The live provider decorator supplies plaintext names for crypt sessions.
    response = await invoke<FileListResponse>('provider_list_files', { path: target.path });
  } else {
    // Legacy list_files has no path argument. Never change cwd to inspect an
    // off-panel destination, or accidentally list the source/current folder.
    if (target.path !== target.visibleRemotePath) {
      throw new Error('Cannot inspect another remote directory on this legacy session');
    }
    response = await invoke<FileListResponse>('list_files');
  }
  if (!target.isProviderSession || target.aeroVaultSessionId) {
    if (response.current_path !== target.path) {
      throw new Error('Remote destination changed while checking overwrite conflicts');
    }
  }
  if (!Array.isArray(response.files)) throw new Error('Invalid remote destination listing');
  return response.files;
}
