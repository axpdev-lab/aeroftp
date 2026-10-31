// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import app from '../App.tsx?raw';
import { DEFAULT_CONFIG, useCircuitBreaker } from '../hooks/useCircuitBreaker';
import { LOCAL, REMOTE, SUB, fixture, local, remote, type Options } from './appTransfers.testkit';

// The circuit breaker keeps its state in refs: outside React, one call of the
// hook is one breaker.
vi.mock('react', async importOriginal => ({
  ...(await importOriginal<typeof import('react')>()),
  useRef: <T,>(value: T) => ({ current: value }),
  useCallback: <T,>(fn: T) => fn,
}));

/**
 * A file that fails is seen by the queue row, the per-file retry and the
 * circuit breaker.
 *
 * uploadFile and downloadFile show an error and answer false, and false is
 * also a skip: the loops that run a selection one file at a time took that
 * answer for a success. The retry, the pause and the reconnect never ran, and
 * the row of a failed file was marked completed unless the backend's error
 * event settled it first. The loops and the queue's Retry now have the error.
 *
 * This runs App's own closures with the IPC mocked and the app's circuit
 * breaker (two retries per file, three errors in a row pause the batch), and
 * a queue that keeps a settled row as the app does.
 */

const breaker = () => useCircuitBreaker({ ...DEFAULT_CONFIG, baseRetryDelay: 0, maxRetryDelay: 0 });
const UPLOAD = 'provider_upload_file';
const DOWNLOAD = 'provider_download_file';

/** One local file dropped on the remote panel: a lone file runs the loop. */
async function upload(options: Options) {
  const f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: breaker(), ...options });
  await f.drop([local('up.txt')], false, REMOTE, 'local');
  return f;
}
const attempts = (f: ReturnType<typeof fixture>, command: string) =>
  f.invoke.mock.calls.filter(([name]) => name === command).length;

describe('a failed file is seen by its queue row, the retry and the circuit breaker', () => {
  it('a file that lands on its second attempt is retried and completed', async () => {
    const f = await upload({ fail: (_command, call) => (call === 1 ? 'Operation timed out' : undefined) });
    expect(attempts(f, UPLOAD)).toBe(2);
    expect(f.row('q0')).toMatchObject({ status: 'completed' });
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  it('a file that keeps failing is retried twice, then its row fails with the error, shown once', async () => {
    const f = await upload({ fail: () => 'Operation timed out' });
    expect(attempts(f, UPLOAD)).toBe(3);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'Operation timed out' });
    expect(f.transferQueue.completeTransfer).not.toHaveBeenCalled();
    expect(f.notify.error).toHaveBeenCalledTimes(1);
  });

  it('an error that a retry cannot fix fails the row at once', async () => {
    const f = await upload({ fail: () => 'No such file or directory' });
    expect(attempts(f, UPLOAD)).toBe(1);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'No such file or directory' });
    expect(f.notify.error).toHaveBeenCalledTimes(1);
  });

  // The backend's error event for the first attempt settles the row, before
  // the retry starts or while it runs: a file the retry lands is completed.
  it.each([['before the retry', 1], ['during the retry', 2]] as const)(
    'an error event of the first attempt %s does not leave a landed file failed',
    async (_when, eventCall) => {
      let f: ReturnType<typeof fixture>;
      let duringRetry: string | undefined;
      const fail = (_command: string, call: number) => {
        if (call === 2) duringRetry = f.row('q0')?.status;
        if (call === eventCall) f.transferQueue.failTransfer('q0', 'Operation timed out');
        return call === 1 ? 'Operation timed out' : undefined;
      };
      f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: breaker(), fail });
      await f.drop([local('up.txt')], false, REMOTE, 'local');
      expect(attempts(f, UPLOAD)).toBe(2);
      // The row reads as transferring while the retry runs, not as failed.
      expect(duringRetry).toBe('transferring');
      expect(f.row('q0')).toMatchObject({ status: 'completed' });
    },
  );

  it('a cancelled batch is not retried, and its file reads as cancelled', async () => {
    let f: ReturnType<typeof fixture>;
    const fail = () => {
      f.batchCancelledRef.current = true;
      return 'Transfer cancelled';
    };
    f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: breaker(), fail });
    await f.drop([local('up.txt')], false, REMOTE, 'local');
    expect(attempts(f, UPLOAD)).toBe(1);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser' });
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  it('a download that fails fails its row', async () => {
    const f = fixture({ folders: { [REMOTE]: [remote('a.txt')] }, breaker: breaker(), fail: () => 'No such file or directory' });
    await f.drop([remote('a.txt')], true, SUB);
    expect(attempts(f, DOWNLOAD)).toBe(1);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'No such file or directory' });
    expect(f.notify.error).toHaveBeenCalledTimes(1);
  });

  it('Retry from the queue fails the row again when the file fails again', async () => {
    const f = await upload({ fail: () => 'No such file or directory' });
    const [retry] = [...f.retryCallbacksRef.current.values()];
    f.transferQueue.retryItem('q0');
    await retry();
    expect(attempts(f, UPLOAD)).toBe(2);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'No such file or directory' });
    expect(f.notify.error).toHaveBeenCalledTimes(2);
  });
});

/**
 * Every place that turns a transfer into a queue row hands the error to the
 * row. A source guard, not a behaviour test: the loop of the file dialog, the
 * Retry of a dialog upload and of a download, the Retry of a row restored from
 * the journal and the folders after a batch are not reached by the cases
 * above, so each `try` that awaits uploadFile or downloadFile and then
 * completes a row is read from App and must pass `'throw'`.
 */
describe('every transfer that completes a queue row passes its error on', () => {
  /** The calls in App that complete a row after they return. */
  function rowCalls(source: string) {
    const found: { call: string; throws: boolean }[] = [];
    for (const match of source.matchAll(/await (?:fns\.)?(?:uploadFile|downloadFile)\(/g)) {
      let depth = 0;
      let end = match.index + match[0].length - 1;
      for (; end < source.length; end++) {
        if (source[end] === '(') depth++;
        if (source[end] === ')' && --depth === 0) break;
      }
      const after = source.slice(end, source.indexOf('catch (', end));
      if (!/transferQueue\.completeTransfer\(/.test(after)) continue;
      const args = source.slice(match.index + match[0].length, end).replace(/\s+/g, ' ').replace(/,\s*$/, '').trim();
      found.push({ call: source.slice(match.index, end + 1).replace(/\s+/g, ' '), throws: args.endsWith("'throw'") });
    }
    return found;
  }

  it('reads a call over several lines, and one that does not pass the error on', () => {
    expect(rowCalls(`try { await fns.uploadFile(\n a,\n b,\n 'throw',\n ); transferQueue.completeTransfer(id); } catch (e) {}`))
      .toEqual([{ call: "await fns.uploadFile( a, b, 'throw', )", throws: true }]);
    expect(rowCalls('try { await downloadFile(a, b); transferQueue.completeTransfer(id); } catch (e) {}'))
      .toEqual([{ call: 'await downloadFile(a, b)', throws: false }]);
    expect(rowCalls('if (await uploadFile(a)) ok += 1; try { x(); } catch (e) {}')).toEqual([]);
  });

  it('the three loops, the four Retry callbacks and the folders after a batch', () => {
    const calls = rowCalls(app);
    expect(calls.filter(c => !c.throws)).toEqual([]);
    // Nine calls: the Retry of a restored row has one for each direction.
    expect(calls).toHaveLength(9);
  });
});

