// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { act, useState } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { DuplicateGroup } from '../types/aerofile';
import { useGuardedClose } from '../hooks/useGuardedClose';

(globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;

type ProgressHandler = (event: { payload: Record<string, unknown> }) => void;

const invokeMock = vi.fn();
let progressListener: ProgressHandler | null = null;

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (_event: string, cb: ProgressHandler) => {
    progressListener = cb;
    return () => {
      if (progressListener === cb) progressListener = null;
    };
  }),
}));

vi.mock('../i18n', () => ({
  useTranslation: () => (key: string, params?: Record<string, unknown>) => {
    if (!params) return key;
    return Object.entries(params).reduce(
      (acc, [k, v]) => acc.replace(`{${k}}`, String(v)),
      `${key}:${JSON.stringify(params)}`,
    );
  },
}));

vi.mock('./ImageThumbnail', () => ({
  ImageThumbnail: ({ name }: { name: string }) => <div data-testid="thumb-img">{name}</div>,
}));

import {
  DuplicateFinderDialog,
  formatScanProgressCounters,
  pruneDuplicateGroups,
  reconcileSelectionAfterPrune,
  retainVisibleSelection,
  type DuplicateScanProgress,
  type LocalPathCheckStatus,
} from './DuplicateFinderDialog';
import { formatBytes } from '../utils/formatters';
import { usePreview } from '../hooks/usePreview';

describe('Find Duplicates enhancements from Discussion #347', () => {
  let host: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
    invokeMock.mockReset();
    progressListener = null;
  });

  afterEach(() => {
    act(() => root.unmount());
    host.remove();
    vi.clearAllMocks();
  });

  // Wish 1: Scan progress counters

  describe('Wish 1: scan progress does not repeat file total and tracks byte progress', () => {
    it('shows files_scanned during walk and processed/total once during analyze', () => {
      const walk: DuplicateScanProgress = {
        scan_id: 's1',
        phase: 'walk',
        files_scanned: 842,
        dirs_scanned: 6,
        bytes_scanned: 120_000_000,
        max_depth: 2,
        files_processed: 0,
        files_total: 0,
        bytes_processed: 0,
        bytes_total: 0,
        files_skipped: 0,
        current_path: '/tmp/a.jpg',
      };
      const walkOut = formatScanProgressCounters(walk);
      expect(walkOut.filesLabel).toBe((842).toLocaleString());
      expect(walkOut.bytesLabel).not.toContain('/');

      const analyze: DuplicateScanProgress = {
        ...walk,
        phase: 'analyze',
        files_scanned: 1931,
        files_processed: 1806,
        files_total: 1931,
        bytes_scanned: 428_300_000,
        bytes_processed: 398_100_000,
        bytes_total: 428_300_000,
      };
      const analyzeOut = formatScanProgressCounters(analyze);
      expect(analyzeOut.filesLabel).toBe(`${(1806).toLocaleString()} / ${(1931).toLocaleString()}`);
      expect(analyzeOut.bytesLabel).toBe(`${formatBytes(398_100_000)} / ${formatBytes(428_300_000)}`);
    });

    it('renders the total file count only once in the scanning counter row during analyze', async () => {
      let resolveScan!: (groups: DuplicateGroup[]) => void;
      let capturedScanId = '';
      invokeMock.mockImplementation((cmd: string, args?: { scanId?: string }) => {
        if (cmd === 'find_duplicate_files') {
          capturedScanId = args?.scanId ?? '';
          return new Promise<DuplicateGroup[]>((resolve) => {
            resolveScan = resolve;
          });
        }
        return Promise.resolve([]);
      });

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/photos"
            onClose={vi.fn()}
            onDeleteFiles={vi.fn(async () => {})}
          />,
        );
      });

      expect(progressListener).toBeTruthy();
      expect(capturedScanId).not.toBe('');

      await act(async () => {
        progressListener!({
          payload: {
            scan_id: capturedScanId,
            phase: 'analyze',
            files_scanned: 1931,
            dirs_scanned: 6,
            bytes_scanned: 428_300_000,
            max_depth: 2,
            files_processed: 1806,
            files_total: 1931,
            bytes_processed: 398_100_000,
            bytes_total: 428_300_000,
            files_skipped: 0,
            current_path: '',
          },
        });
      });

      const stripText = host.querySelector('[data-testid="duplicates-scan-counters"]')?.textContent ?? '';
      const totalFormatted = (1931).toLocaleString();
      const occurrences = stripText.split(totalFormatted).length - 1;
      expect(occurrences).toBe(1);
      expect(host.querySelector('[data-testid="duplicates-scan-files"]')?.textContent).toContain(
        `${(1806).toLocaleString()} / ${totalFormatted}`,
      );
      expect(host.querySelector('[data-testid="duplicates-scan-bytes"]')?.textContent?.trim()).toBe(
        `${formatBytes(398_100_000)} / ${formatBytes(428_300_000)}`,
      );

      await act(async () => {
        resolveScan([]);
      });
    });
  });

  // Wish 2: Fullscreen preview byte size

  describe('Wish 2: clicking an image thumbnail passes its real byte size to preview', () => {
    it('passes the file byte size to onPreviewFile from per-file sizes', async () => {
      const groups: DuplicateGroup[] = [
        {
          hash: 'abc123',
          size: 204800,
          files: ['/photos/a.jpg', '/photos/a (1).jpg'],
          file_sizes: [204800, 198400],
        },
      ];
      invokeMock.mockResolvedValueOnce(groups);
      const onPreviewFile = vi.fn();

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/photos"
            onClose={vi.fn()}
            onDeleteFiles={vi.fn(async () => {})}
            onPreviewFile={onPreviewFile}
          />,
        );
      });

      const thumbs = host.querySelectorAll('button[title="contextMenu.preview"]');
      expect(thumbs.length).toBe(2);

      await act(async () => {
        thumbs[0].dispatchEvent(new MouseEvent('click', { bubbles: true }));
        thumbs[1].dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

      expect(onPreviewFile).toHaveBeenNthCalledWith(1, '/photos/a.jpg', 204800);
      expect(onPreviewFile).toHaveBeenNthCalledWith(2, '/photos/a (1).jpg', 198400);
    });

    it('derives the actual byte size in usePreview when opened with size: null (image and text)', async () => {
      const imageBytes = new Uint8Array(1234).fill(0x5a);
      const imageBase64 = Buffer.from(imageBytes).toString('base64');
      const utf8Text = 'hello cafe \u00e9\u20ac';
      const utf8ByteLength = new TextEncoder().encode(utf8Text).byteLength;

      const origCreateObjectURL = URL.createObjectURL;
      const origRevokeObjectURL = URL.revokeObjectURL;
      const origDecode = Image.prototype.decode;
      URL.createObjectURL = vi.fn(() => 'blob:mock-url');
      URL.revokeObjectURL = vi.fn();
      Image.prototype.decode = vi.fn(async () => {});

      try {
        invokeMock.mockImplementation(async (cmd: string) => {
          if (cmd === 'read_local_file_base64') return imageBase64;
          if (cmd === 'read_local_file') return utf8Text;
          if (cmd === 'get_local_files') return [];
          return null;
        });

        let previewApi!: ReturnType<typeof usePreview>;
        const Harness: React.FC = () => {
          previewApi = usePreview({
            notify: { error: vi.fn(), info: vi.fn() },
            toast: { removeToast: vi.fn() },
            remoteTarget: {
              snapshot: () => ({ serverId: null, accountId: null, overlayId: null }) as never,
              changedSince: () => false,
              changedMessage: () => 'changed',
            },
          });
          return <div data-testid="preview-size">{previewApi.universalPreviewFile?.size ?? 'none'}</div>;
        };

        await act(async () => {
          root.render(<Harness />);
        });

        await act(async () => {
          await previewApi.openUniversalPreview(
            { name: 'photo.png', path: '/photos/photo.png', size: null, is_dir: false, modified: null },
            false,
            [],
          );
        });
        expect(previewApi.universalPreviewFile?.size).toBe(1234);

        await act(async () => {
          await previewApi.openUniversalPreview(
            { name: 'notes.txt', path: '/photos/notes.txt', size: null, is_dir: false, modified: null },
            false,
            [],
          );
        });
        expect(previewApi.universalPreviewFile?.size).toBe(utf8ByteLength);
      } finally {
        URL.createObjectURL = origCreateObjectURL;
        URL.revokeObjectURL = origRevokeObjectURL;
        Image.prototype.decode = origDecode;
      }
    });
  });

  // Wish 3: Refresh without rescanning

  describe('Wish 3: Refresh button checks existing duplicate paths without rescanning', () => {
    it('pruneDuplicateGroups removes missing files, preserves parallel arrays, and drops singletons', () => {
      const groups: DuplicateGroup[] = [
        {
          hash: 'g1',
          size: 100,
          files: ['/a.png', '/b.png', '/c.png'],
          file_hashes: ['h1', 'h2', 'h3'],
          file_sizes: [100, 200, 300],
        },
        {
          hash: 'g2',
          size: 50,
          files: ['/x.png', '/y.png'],
          file_hashes: ['hx', 'hy'],
          file_sizes: [50, 55],
        },
      ];
      const existing = new Set(['/b.png', '/c.png', '/y.png']);
      const pruned = pruneDuplicateGroups(groups, (f) => existing.has(f));

      expect(pruned).toEqual([
        {
          hash: 'g1',
          size: 200,
          files: ['/b.png', '/c.png'],
          file_hashes: ['h2', 'h3'],
          file_sizes: [200, 300],
        },
      ]);
    });

    it('invokes filter_existing_local_paths (not find_duplicate_files), disables Delete while refreshing, and clears missing + orphaned selections', async () => {
      const initialGroups: DuplicateGroup[] = [
        {
          hash: 'g1',
          size: 1000,
          files: ['/dir/photo.jpg', '/dir/photo (1).jpg'],
          file_sizes: [1000, 1000],
        },
        {
          hash: 'g2',
          size: 2000,
          files: ['/dir/doc.txt', '/dir/doc (1).txt', '/dir/doc (2).txt'],
          file_sizes: [2000, 2000, 2000],
        },
      ];

      let resolveCheck!: (statuses: LocalPathCheckStatus[]) => void;
      invokeMock.mockImplementation((cmd: string) => {
        if (cmd === 'find_duplicate_files') return Promise.resolve(initialGroups);
        if (cmd === 'filter_existing_local_paths') {
          return new Promise<LocalPathCheckStatus[]>((resolve) => {
            resolveCheck = resolve;
          });
        }
        return Promise.resolve([]);
      });

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={vi.fn(async () => {})}
          />,
        );
      });

      // Initially: g1 has '/dir/photo (1).jpg' ticked; g2 has '(1)' and '(2)' ticked => 3 selected.
      expect(host.textContent).toContain('(3)');

      const refreshBtn = host.querySelector('[data-testid="duplicates-refresh-btn"]') as HTMLButtonElement;
      const deleteBtn = Array.from(host.querySelectorAll('button')).find((b) =>
        b.textContent?.includes('duplicates.deleteSelected'),
      ) as HTMLButtonElement;
      expect(refreshBtn).toBeTruthy();
      expect(deleteBtn).toBeTruthy();
      expect(deleteBtn.disabled).toBe(false);

      await act(async () => {
        refreshBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      // While refresh is in flight, Delete must be disabled so clicks are not silently swallowed.
      expect(deleteBtn.disabled).toBe(true);

      await act(async () => {
        // User deleted '/dir/photo.jpg' (the kept copy in g1) and '/dir/doc (1).txt' externally.
        resolveCheck([
          { status: 'missing', path: '/dir/photo.jpg' },
          { status: 'present', path: '/dir/photo (1).jpg', size: 1000 },
          { status: 'present', path: '/dir/doc.txt', size: 2000 },
          { status: 'missing', path: '/dir/doc (1).txt' },
          { status: 'present', path: '/dir/doc (2).txt', size: 2000 },
        ]);
      });
      expect(deleteBtn.disabled).toBe(false);

      // Must NOT have rescanned the folder.
      const scanCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === 'find_duplicate_files');
      const checkCalls = invokeMock.mock.calls.filter(([cmd]) => cmd === 'filter_existing_local_paths');
      expect(scanCalls).toHaveLength(1);
      expect(checkCalls).toHaveLength(1);

      // g1 dropped (only 1 copy left), so its previously ticked copy '/dir/photo (1).jpg' is cleared from selection.
      // g2 survives with 2 copies ('/dir/doc.txt' kept, '/dir/doc (2).txt' ticked) => 1 selected.
      expect(host.textContent).toContain('(1)');
      expect(host.textContent).not.toContain('photo (1).jpg');
      expect(host.textContent).toContain('doc (2).txt');
      expect(host.querySelector('[data-testid="duplicates-refresh-notice"]')?.textContent).toContain('2');
    });

    it('keeps unverified paths without dropping them or claiming all files still exist (C3-3)', async () => {
      const initialGroups: DuplicateGroup[] = [
        {
          hash: 'g-perm',
          size: 1000,
          files: ['/dir/locked.jpg', '/dir/locked (1).jpg'],
          file_sizes: [1000, 1000],
        },
      ];
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === 'find_duplicate_files') return initialGroups;
        if (cmd === 'filter_existing_local_paths') {
          return [
            { status: 'unverified', path: '/dir/locked.jpg' },
            { status: 'present', path: '/dir/locked (1).jpg', size: 1000 },
          ] satisfies LocalPathCheckStatus[];
        }
        return [];
      });

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={vi.fn(async () => {})}
          />,
        );
      });

      const refreshBtn = host.querySelector('[data-testid="duplicates-refresh-btn"]') as HTMLButtonElement;
      await act(async () => {
        refreshBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

      // Row stays visible and notice reports unverified count (never upToDate).
      expect(host.textContent).toContain('locked.jpg');
      const notice = host.querySelector('[data-testid="duplicates-refresh-notice"]')?.textContent ?? '';
      expect(notice).toContain('duplicates.refreshUnverified');
      expect(notice).toContain('1');
      expect(notice).not.toContain('duplicates.refreshUpToDate');
    });

    it('drops a resized file and its tick on Refresh and reports changed count (C3-6)', async () => {
      const initialGroups: DuplicateGroup[] = [
        {
          hash: 'g-resize',
          size: 1000,
          files: ['/dir/pic.jpg', '/dir/pic (1).jpg'],
          file_sizes: [1000, 1000],
        },
        {
          hash: 'g-keep',
          size: 2000,
          files: ['/dir/same.txt', '/dir/same (1).txt'],
          file_sizes: [2000, 2000],
        },
      ];
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === 'find_duplicate_files') return initialGroups;
        if (cmd === 'filter_existing_local_paths') {
          return [
            { status: 'present', path: '/dir/pic.jpg', size: 1000 },
            // Replaced with a file of different size at the same path
            { status: 'present', path: '/dir/pic (1).jpg', size: 1550 },
            { status: 'present', path: '/dir/same.txt', size: 2000 },
            { status: 'present', path: '/dir/same (1).txt', size: 2000 },
          ] satisfies LocalPathCheckStatus[];
        }
        return [];
      });

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={vi.fn(async () => {})}
          />,
        );
      });

      expect(host.textContent).toContain('(2)');
      const refreshBtn = host.querySelector('[data-testid="duplicates-refresh-btn"]') as HTMLButtonElement;
      await act(async () => {
        refreshBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

      // Resized copy and its now-singleton group are dropped, tick cleared -> (1) remains.
      expect(host.textContent).not.toContain('pic (1).jpg');
      expect(host.textContent).toContain('same (1).txt');
      expect(host.textContent).toContain('(1)');
      const notice = host.querySelector('[data-testid="duplicates-refresh-notice"]')?.textContent ?? '';
      expect(notice).toContain('duplicates.refreshChanged');
      expect(notice).toContain('1');
    });

    it('keeps results and selection on Refresh failure and retries without rescanning (C3-4)', async () => {
      const initialGroups: DuplicateGroup[] = [
        {
          hash: 'g-err',
          size: 1000,
          files: ['/dir/a.jpg', '/dir/a (1).jpg'],
          file_sizes: [1000, 1000],
        },
      ];
      let checkAttempt = 0;
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === 'find_duplicate_files') return initialGroups;
        if (cmd === 'filter_existing_local_paths') {
          checkAttempt += 1;
          if (checkAttempt === 1) throw new Error('disk busy');
          return [
            { status: 'present', path: '/dir/a.jpg', size: 1000 },
            { status: 'present', path: '/dir/a (1).jpg', size: 1000 },
          ] satisfies LocalPathCheckStatus[];
        }
        return [];
      });

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={vi.fn(async () => {})}
          />,
        );
      });

      const refreshBtn = () =>
        host.querySelector('[data-testid="duplicates-refresh-btn"]') as HTMLButtonElement;

      // First Refresh fails: rows and selection stay visible, inline error shown.
      await act(async () => {
        refreshBtn().dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(host.textContent).toContain('a (1).jpg');
      expect(host.textContent).toContain('(1)');
      const errNotice = host.querySelector('[data-testid="duplicates-refresh-notice"]')?.textContent ?? '';
      expect(errNotice).toContain('duplicates.refreshFailed');
      expect(errNotice).toContain('disk busy');

      // Retry Refresh succeeds without ever calling find_duplicate_files again.
      await act(async () => {
        refreshBtn().dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(host.querySelector('[data-testid="duplicates-refresh-notice"]')?.textContent).toContain(
        'duplicates.refreshUpToDate',
      );
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'find_duplicate_files')).toHaveLength(1);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'filter_existing_local_paths')).toHaveLength(2);
    });

    it('disables Refresh while delete confirmation is open and blocks Confirm during pending Refresh (C3-8)', async () => {
      const initialGroups: DuplicateGroup[] = [
        {
          hash: 'g-modal',
          size: 1000,
          files: ['/dir/x.jpg', '/dir/x (1).jpg'],
          file_sizes: [1000, 1000],
        },
      ];
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === 'find_duplicate_files') return initialGroups;
        return [
          { status: 'present', path: '/dir/x.jpg', size: 1000 },
          { status: 'present', path: '/dir/x (1).jpg', size: 1000 },
        ] satisfies LocalPathCheckStatus[];
      });
      const onDeleteFiles = vi.fn(async () => {});

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={onDeleteFiles}
            confirmBeforeDelete={true}
          />,
        );
      });

      const deleteBtn = Array.from(host.querySelectorAll('button')).find((b) =>
        b.textContent?.includes('duplicates.deleteSelected'),
      ) as HTMLButtonElement;
      const refreshBtn = host.querySelector('[data-testid="duplicates-refresh-btn"]') as HTMLButtonElement;
      expect(refreshBtn.disabled).toBe(false);

      // Open delete confirmation: Refresh button underneath must become disabled and inert.
      await act(async () => {
        deleteBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(host.textContent).toContain('duplicates.deleteConfirm');
      expect(refreshBtn.disabled).toBe(true);
      await act(async () => {
        refreshBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === 'filter_existing_local_paths')).toHaveLength(0);
      expect(onDeleteFiles).not.toHaveBeenCalled();
    });
  });

  // Wish 4: Esc closes preview / inline confirm first, then Find Duplicates

  describe('Wish 4: Escape closes fullscreen preview or inline confirm first, then Find Duplicates', () => {
    it('leaves Find Duplicates open when Esc closes an overlay using useGuardedClose', async () => {
      const groups: DuplicateGroup[] = [
        {
          hash: 'img1',
          size: 4096,
          files: ['/img/one.png', '/img/two.png'],
          file_sizes: [4096, 4096],
        },
      ];
      invokeMock.mockResolvedValue(groups);

      const onCloseFinder = vi.fn();
      const onClosePreview = vi.fn();

      const OverlayPreview: React.FC<{ isOpen: boolean; onClose: () => void }> = ({ isOpen, onClose }) => {
        useGuardedClose({ guard: null, onClose, escape: isOpen });
        return isOpen ? <div data-testid="fake-preview">Preview</div> : null;
      };

      const Harness: React.FC = () => {
        const [finderOpen, setFinderOpen] = useState(true);
        const [previewOpen, setPreviewOpen] = useState(false);
        return (
          <>
            <DuplicateFinderDialog
              isOpen={finderOpen}
              scanPath="/img"
              onClose={() => {
                onCloseFinder();
                setFinderOpen(false);
              }}
              onDeleteFiles={vi.fn(async () => {})}
              onPreviewFile={() => setPreviewOpen(true)}
            />
            <OverlayPreview
              isOpen={previewOpen}
              onClose={() => {
                onClosePreview();
                setPreviewOpen(false);
              }}
            />
          </>
        );
      };

      await act(async () => {
        root.render(<Harness />);
      });

      const thumb = host.querySelector('button[title="contextMenu.preview"]') as HTMLButtonElement;
      expect(thumb).toBeTruthy();

      await act(async () => {
        thumb.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(host.querySelector('[data-testid="fake-preview"]')).toBeTruthy();

      // First Esc: closes ONLY the preview; Find Duplicates stays open.
      await act(async () => {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
      });
      expect(onClosePreview).toHaveBeenCalledTimes(1);
      expect(onCloseFinder).toHaveBeenCalledTimes(0);
      expect(host.querySelector('[data-testid="fake-preview"]')).toBeNull();
      expect(host.querySelector('[role="dialog"]')).toBeTruthy();

      // Second Esc: now from Find Duplicates itself, closes Find Duplicates.
      await act(async () => {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
      });
      expect(onCloseFinder).toHaveBeenCalledTimes(1);
      expect(host.querySelector('[role="dialog"]')).toBeNull();
    });

    it('suspends the dialog Escape handler while the inline delete confirmation is open', async () => {
      const groups: DuplicateGroup[] = [
        {
          hash: 'img2',
          size: 4096,
          files: ['/img/a.png', '/img/a (1).png'],
          file_sizes: [4096, 4096],
        },
      ];
      invokeMock.mockResolvedValue(groups);
      const onCloseFinder = vi.fn();

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/img"
            onClose={onCloseFinder}
            onDeleteFiles={vi.fn(async () => {})}
            confirmBeforeDelete={true}
          />,
        );
      });

      const deleteBtn = Array.from(host.querySelectorAll('button')).find((b) =>
        b.textContent?.includes('duplicates.deleteSelected'),
      ) as HTMLButtonElement;
      expect(deleteBtn).toBeTruthy();

      await act(async () => {
        deleteBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(host.textContent).toContain('duplicates.deleteConfirm');

      // First Esc dismisses the confirmation without closing Find Duplicates.
      await act(async () => {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
      });
      expect(host.textContent).not.toContain('duplicates.deleteConfirm');
      expect(onCloseFinder).toHaveBeenCalledTimes(0);

      // Second Esc closes Find Duplicates.
      await act(async () => {
        window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, cancelable: true }));
      });
      expect(onCloseFinder).toHaveBeenCalledTimes(1);
    });
  });

  describe('Full-group delete safety guard', () => {
    it('confirms when Select All ticks every copy even with confirmBeforeDelete off', async () => {
      const groups: DuplicateGroup[] = [
        {
          hash: 'g-all',
          size: 2048,
          files: ['/dir/only.png', '/dir/only (1).png'],
          file_sizes: [2048, 2048],
        },
      ];
      invokeMock.mockResolvedValueOnce(groups);
      const onDeleteFiles = vi.fn(async () => {});

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={onDeleteFiles}
            confirmBeforeDelete={false}
          />,
        );
      });

      const selectAllBtn = Array.from(host.querySelectorAll('button')).find(
        (b) => b.textContent?.trim() === 'duplicates.selectAll',
      ) as HTMLButtonElement;
      const deleteBtn = Array.from(host.querySelectorAll('button')).find((b) =>
        b.textContent?.includes('duplicates.deleteSelected'),
      ) as HTMLButtonElement;
      expect(selectAllBtn).toBeTruthy();
      expect(deleteBtn).toBeTruthy();

      await act(async () => {
        selectAllBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      await act(async () => {
        deleteBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

      expect(host.textContent).toContain('duplicates.deleteConfirm');
      expect(host.textContent).toContain('duplicates.everyCopyTicked');
      expect(onDeleteFiles).not.toHaveBeenCalled();

      const confirmDeleteBtn = Array.from(host.querySelectorAll('button')).find(
        (b) => b.textContent?.trim() === 'common.delete',
      ) as HTMLButtonElement;
      expect(confirmDeleteBtn).toBeTruthy();

      await act(async () => {
        confirmDeleteBtn.dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(onDeleteFiles).toHaveBeenCalledTimes(1);
      expect(onDeleteFiles).toHaveBeenCalledWith(['/dir/only.png', '/dir/only (1).png']);
    });

    it('drops an orphaned tick on the last surviving copy when a deferred delete resolves', async () => {
      const groups: DuplicateGroup[] = [
        {
          hash: 'g1',
          size: 1000,
          files: ['/dir/a.png', '/dir/a (1).png'],
          file_sizes: [1000, 1000],
        },
        {
          hash: 'g2',
          size: 2000,
          files: ['/dir/c.png', '/dir/c (1).png'],
          file_sizes: [2000, 2000],
        },
      ];
      invokeMock.mockResolvedValueOnce(groups);

      let resolveFirstDelete!: () => void;
      const onDeleteFiles = vi
        .fn<(paths: string[]) => Promise<void>>()
        .mockImplementationOnce(
          () =>
            new Promise<void>((resolve) => {
              resolveFirstDelete = resolve;
            }),
        )
        .mockResolvedValue(undefined);

      await act(async () => {
        root.render(
          <DuplicateFinderDialog
            isOpen={true}
            scanPath="/dir"
            onClose={vi.fn()}
            onDeleteFiles={onDeleteFiles}
            confirmBeforeDelete={false}
          />,
        );
      });

      const checkboxFor = (fileName: string): HTMLElement => {
        const nameSpan = Array.from(host.querySelectorAll('span[title]')).find(
          (s) => s.getAttribute('title') === fileName,
        );
        const row = nameSpan?.closest('div.flex.items-start');
        const box = row?.querySelector('[role="checkbox"]') as HTMLElement | null;
        if (!box) throw new Error(`checkbox not found for ${fileName}`);
        return box;
      };

      // Untick /dir/c (1).png so only /dir/a (1).png is selected.
      await act(async () => {
        checkboxFor('c (1).png').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

      const deleteBtn = () =>
        Array.from(host.querySelectorAll('button')).find((b) =>
          b.textContent?.includes('duplicates.deleteSelected'),
        ) as HTMLButtonElement;
      expect(deleteBtn().textContent).toContain('(1)');

      // Start deleting /dir/a (1).png; promise stays pending.
      await act(async () => {
        deleteBtn().dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(onDeleteFiles).toHaveBeenCalledTimes(1);
      expect(onDeleteFiles).toHaveBeenLastCalledWith(['/dir/a (1).png']);

      // While the delete is pending, tick /dir/a.png (the only copy left in g1).
      await act(async () => {
        checkboxFor('a.png').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });

      // Now the first delete finishes and drops g1 because only /dir/a.png remains.
      await act(async () => {
        resolveFirstDelete();
      });

      // /dir/a.png must NOT remain selected: Delete button shows (0) and is disabled.
      expect(host.textContent).not.toContain('a.png');
      expect(deleteBtn().textContent).toContain('(0)');
      expect(deleteBtn().disabled).toBe(true);

      // Even if the user now ticks /dir/c (1).png and deletes, /dir/a.png must NEVER be passed to onDeleteFiles.
      await act(async () => {
        checkboxFor('c (1).png').dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(deleteBtn().textContent).toContain('(1)');
      await act(async () => {
        deleteBtn().dispatchEvent(new MouseEvent('click', { bubbles: true }));
      });
      expect(onDeleteFiles).toHaveBeenCalledTimes(2);
      expect(onDeleteFiles).toHaveBeenLastCalledWith(['/dir/c (1).png']);
    });

    it('reconcileSelectionAfterPrune drops orphaned ticks while keeping ticks on surviving groups', () => {
      const surviving: DuplicateGroup[] = [
        { hash: 'g2', size: 2000, files: ['/dir/c.png', '/dir/c (1).png'] },
      ];
      const selected = new Set(['/dir/a.png', '/dir/a (1).png', '/dir/c (1).png']);
      const next = reconcileSelectionAfterPrune(selected, surviving, ['/dir/a (1).png']);
      expect(next).toEqual(new Set(['/dir/c (1).png']));
    });

    it('retainVisibleSelection strips any stray path outside current groups', () => {
      const current: DuplicateGroup[] = [
        { hash: 'g2', size: 2000, files: ['/dir/c.png', '/dir/c (1).png'] },
      ];
      const next = retainVisibleSelection(new Set(['/stray/last-copy.png', '/dir/c (1).png']), current);
      expect(next).toEqual(new Set(['/dir/c (1).png']));
    });
  });
});
