import { describe, expect, it, vi } from 'vitest';
import { isDeletionCancelled, trashLocalPaths, TRASH_NEEDS_HOME_COPY, type LocalTrashDeps } from './localTrash';

type Call = { cmd: string; args: Record<string, unknown> };

/** A backend where each path answers `delete_to_trash` as scripted. */
function backend(trashAnswers: Record<string, 'ok' | 'copy' | 'fail'>) {
  const calls: Call[] = [];
  const invoke = vi.fn(async (cmd: string, args: Record<string, unknown>) => {
    calls.push({ cmd, args });
    const path = String(args.path);
    if (cmd === 'delete_to_trash' && !args.allowHomeCopy) {
      const answer = trashAnswers[path];
      if (answer === 'copy') throw `${TRASH_NEEDS_HOME_COPY}: ${path} is on /media/x`;
      if (answer === 'fail') throw 'Failed to move to trash: no trash here';
    }
    return null;
  });
  return { calls, invoke };
}

const permanentCalls = (calls: Call[]) =>
  calls.filter(c => c.cmd === 'delete_local_file').map(c => c.args.path);

describe('trashLocalPaths', () => {
  it('asks once for every item that would be copied into the home trash', async () => {
    const { calls, invoke } = backend({ '/a': 'ok', '/m/b': 'copy', '/m/c': 'copy' });
    const askHomeCopy = vi.fn(async () => 'copy' as const);
    const result = await trashLocalPaths(['/a', '/m/b', '/m/c'], { invoke, askHomeCopy });

    expect(askHomeCopy).toHaveBeenCalledTimes(1);
    expect(askHomeCopy).toHaveBeenCalledWith(['/m/b', '/m/c']);
    expect(calls.filter(c => c.args.allowHomeCopy === true).map(c => c.args.path)).toEqual(['/m/b', '/m/c']);
    expect(result.removed).toEqual(['/a', '/m/b', '/m/c']);
    expect(permanentCalls(calls)).toEqual([]);
  });

  it('keeps the items when the user cancels, and deletes nothing', async () => {
    const { calls, invoke } = backend({ '/m/b': 'copy' });
    const result = await trashLocalPaths(['/m/b'], { invoke, askHomeCopy: async () => 'cancel' });

    expect(result.kept).toEqual(['/m/b']);
    expect(result.removed).toEqual([]);
    expect(permanentCalls(calls)).toEqual([]);
  });

  it('names the panel gesture on every permanent delete, so its Cancel reaches the one in flight', async () => {
    const { calls, invoke } = backend({ '/m/b': 'copy', '/m/c': 'fail' });
    await trashLocalPaths(['/m/b', '/m/c'], {
      invoke,
      askHomeCopy: async () => 'permanent',
      askPermanentAfterFailure: async () => true,
      operationId: 'gesture-1',
    });
    const permanent = calls.filter(c => c.cmd === 'delete_local_file');
    expect(permanent.map(c => c.args)).toEqual([
      { path: '/m/b', operationId: 'gesture-1' },
      { path: '/m/c', operationId: 'gesture-1' },
    ]);
  });

  it('reads a cancelled delete in both shapes, and only those', () => {
    expect(isDeletionCancelled('Deletion cancelled: 0 of 1 files deleted')).toBe(true);
    expect(isDeletionCancelled('Deletion cancelled')).toBe(true);
    expect(isDeletionCancelled('Transfer cancelled by user [sync-error:unknown:false]')).toBe(true);
    expect(isDeletionCancelled(new Error('Transfer cancelled by user'))).toBe(true);
    for (const other of [undefined, null, 'Deleted', 'Failed to delete: no such file', new Error('denied'), 3]) {
      expect(isDeletionCancelled(other)).toBe(false);
    }
  });

  it('keeps every item from a cancelled permanent delete on, in either shape (one reader)', async () => {
    for (const cancelled of ['resolved', 'rejected'] as const) {
      const calls: Call[] = [];
      const invoke = vi.fn(async (cmd: string, args: Record<string, unknown>) => {
        calls.push({ cmd, args });
        if (cmd === 'delete_to_trash' && !args.allowHomeCopy) throw 'Failed to move to trash: no trash here';
        if (cmd === 'delete_local_file') {
          if (cancelled === 'resolved') return 'Deletion cancelled: 2 of 5 files deleted';
          throw 'Transfer cancelled by user [sync-error:unknown:false]';
        }
        return null;
      });
      const result = await trashLocalPaths(['/a', '/b', '/c'], {
        invoke,
        askHomeCopy: async () => 'permanent',
        askPermanentAfterFailure: async () => true,
      });
      expect(permanentCalls(calls)).toEqual(['/a']);
      expect(result.removed).toEqual([]);
      expect(result.failed).toEqual([]);
      expect(result.kept).toEqual(['/a', '/b', '/c']);
    }
  });

  it('a cancelled delete in one group stops the other too, with no isCancelled and in either shape (M7)', async () => {
    for (const cancelled of ['resolved', 'rejected'] as const) {
      const calls: Call[] = [];
      const askPermanentAfterFailure = vi.fn(async () => true);
      const invoke = vi.fn(async (cmd: string, args: Record<string, unknown>) => {
        calls.push({ cmd, args });
        const path = String(args.path);
        if (cmd === 'delete_to_trash' && !args.allowHomeCopy) {
          if (path.startsWith('/m/')) throw `${TRASH_NEEDS_HOME_COPY}: ${path} is on /media/x`;
          throw 'Failed to move to trash: no trash here';
        }
        if (cmd === 'delete_local_file') {
          if (cancelled === 'resolved') return 'Deletion cancelled: 0 of 1 files deleted';
          throw 'Transfer cancelled by user [sync-error:unknown:false]';
        }
        return null;
      });
      // One path needs a home copy, two others have another trash failure.
      const result = await trashLocalPaths(['/m/a', '/r/b', '/r/c'], {
        invoke,
        askHomeCopy: async () => 'permanent',
        askPermanentAfterFailure,
      });
      expect(permanentCalls(calls)).toEqual(['/m/a']);
      expect(askPermanentAfterFailure).not.toHaveBeenCalled();
      expect(result.kept.sort()).toEqual(['/m/a', '/r/b', '/r/c']);
      expect(result.removed).toEqual([]);
      expect(result.failed).toEqual([]);
    }
  });

  it('deletes permanently only when the user chose it', async () => {
    const { calls, invoke } = backend({ '/m/b': 'copy' });
    const result = await trashLocalPaths(['/m/b'], { invoke, askHomeCopy: async () => 'permanent' });

    expect(permanentCalls(calls)).toEqual(['/m/b']);
    expect(result.removed).toEqual(['/m/b']);
  });

  // Pre-release review of 4.2.1 (L3): any answer other than 'copy' or
  // 'cancel' fell through to the permanent delete. A dialog that resolves
  // nothing on dismiss (or a value added later) must keep the items.
  it.each([undefined, null, '', 'dismissed', 'PERMANENT'])(
    'keeps the items and deletes nothing when the answer is %j',
    async answer => {
      const { calls, invoke } = backend({ '/m/b': 'copy' });
      const result = await trashLocalPaths(['/m/b'], {
        invoke,
        askHomeCopy: async () => answer as unknown as 'cancel',
      });

      expect(permanentCalls(calls)).toEqual([]);
      expect(calls.filter(c => c.args.allowHomeCopy === true)).toEqual([]);
      expect(result.kept).toEqual(['/m/b']);
      expect(result.removed).toEqual([]);
    },
  );

  it('deletes after a trash failure only on an explicit yes', async () => {
    const { calls, invoke } = backend({ '/x': 'fail' });
    const result = await trashLocalPaths(['/x'], {
      invoke,
      askHomeCopy: async () => 'copy',
      askPermanentAfterFailure: async () => 'yes' as unknown as boolean,
    });

    expect(permanentCalls(calls)).toEqual([]);
    expect(result.kept).toEqual(['/x']);
  });

  it('never turns a trash failure into a permanent delete without asking', async () => {
    const { calls, invoke } = backend({ '/x': 'fail' });
    const deps: LocalTrashDeps = { invoke, askHomeCopy: async () => 'copy' };
    const result = await trashLocalPaths(['/x'], deps);

    expect(permanentCalls(calls)).toEqual([]);
    expect(result.failed.map(f => f.path)).toEqual(['/x']);
  });

  it('offers the permanent delete for trash failures when the caller can ask, and honours a no', async () => {
    const { calls, invoke } = backend({ '/x': 'fail', '/y': 'fail' });
    const ask = vi.fn(async () => false);
    const result = await trashLocalPaths(['/x', '/y'], {
      invoke,
      askHomeCopy: async () => 'copy',
      askPermanentAfterFailure: ask,
    });

    expect(ask).toHaveBeenCalledWith(['/x', '/y']);
    expect(permanentCalls(calls)).toEqual([]);
    expect(result.kept).toEqual(['/x', '/y']);
  });

  it('does not read the marker out of an ordinary error that quotes it', async () => {
    const invoke = vi.fn(async (cmd: string, args: Record<string, unknown>) => {
      if (cmd === 'delete_to_trash') throw `Failed to move to trash: permission denied on /data/${TRASH_NEEDS_HOME_COPY}/x`;
      return args;
    });
    const askHomeCopy = vi.fn(async () => 'copy' as const);
    const result = await trashLocalPaths(['/data/TRASH_NEEDS_HOME_COPY/x'], { invoke, askHomeCopy });

    expect(askHomeCopy).not.toHaveBeenCalled();
    expect(result.failed.map(f => f.path)).toEqual(['/data/TRASH_NEEDS_HOME_COPY/x']);
  });

  it('stops the deferred copies or deletes once the operation is cancelled', async () => {
    const { calls, invoke } = backend({ '/m/a': 'copy', '/m/b': 'copy', '/m/c': 'copy' });
    let cancelled = false;
    const wrapped = vi.fn(async (cmd: string, args: Record<string, unknown>) => {
      const out = await invoke(cmd, args);
      if (args.allowHomeCopy) cancelled = true; // cancel during the first copy
      return out;
    });
    const result = await trashLocalPaths(['/m/a', '/m/b', '/m/c'], {
      invoke: wrapped,
      askHomeCopy: async () => 'copy',
      isCancelled: () => cancelled,
    });

    expect(calls.filter(c => c.args.allowHomeCopy === true).map(c => c.args.path)).toEqual(['/m/a']);
    expect(result.removed).toEqual(['/m/a']);
    expect(result.kept).toEqual(['/m/b', '/m/c']);
  });
});
