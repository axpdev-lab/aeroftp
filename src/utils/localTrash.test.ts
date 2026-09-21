import { describe, expect, it, vi } from 'vitest';
import { trashLocalPaths, TRASH_NEEDS_HOME_COPY, type LocalTrashDeps } from './localTrash';

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

  it('deletes permanently only when the user chose it', async () => {
    const { calls, invoke } = backend({ '/m/b': 'copy' });
    const result = await trashLocalPaths(['/m/b'], { invoke, askHomeCopy: async () => 'permanent' });

    expect(permanentCalls(calls)).toEqual(['/m/b']);
    expect(result.removed).toEqual(['/m/b']);
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
});
