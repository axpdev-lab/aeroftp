// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { parseSync } from 'rolldown/experimental';
import { transformWithOxc } from 'vite';
import { splitForFileBatch } from '../utils/fileBatchRouting';
import { usesProviderApi } from '../types';
import { isCancelledTransferError } from '../utils/transferQueueJournal';
import app from '../App.tsx?raw';
import { DEFAULT_CONFIG, useCircuitBreaker } from '../hooks/useCircuitBreaker';
import { INBOX, LOCAL, REMOTE, SUB, fixture, folder, local, remote, type Options } from './appTransfers.testkit';

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
 * breaker (download retries, three errors in a row pause the batch), and
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

// Exercise the file-picker branch of the actual App closure too. The shared
// fixture reaches selection/drop; this branch starts with no panel selection.
const pickerStart = app.indexOf('  const uploadMultipleFiles = withBatchLatch(');
const pickerEnd = app.indexOf('  // === Bulk Operations ===', pickerStart);
const { code: pickerCode } = await transformWithOxc(
  app.slice(pickerStart, pickerEnd) + '\nreturn uploadMultipleFiles;', 'upload-picker.ts',
);
async function uploadPicked(f: ReturnType<typeof fixture>, circuit: ReturnType<typeof breaker>, picked = [`${LOCAL}/up.txt`], onPick = () => {}) {
  const context = {
    withBatchLatch: <T,>(fn: T) => fn, batchGenerationRef: { current: 0 }, isConnected: true, focusedLocalPanelId: 'local', currentRemotePath: REMOTE,
    getActiveLocalState: () => f.panels.local, resetOverwriteSettings: vi.fn(),
    pickFile: vi.fn(async () => { onPick(); return picked; }), t: (key: string) => key,
    splitForFileBatch, usesProviderApi, liveConnectionParams: f.liveConnectionParams, loadRemoteFiles: f.loadRemoteFiles,
    batchCancelledRef: f.batchCancelledRef, stopCountRef: f.stopCountRef, cancelLevelRef: { current: 0 }, circuitBreaker: circuit,
    invoke: f.invoke, settings: { autoStartTransfers: true }, sessions: [{ id: 'A' }], activeSessionId: 'A',
    transferQueue: f.transferQueue, recordQueueDescriptor: vi.fn(), retryCallbacksRef: f.retryCallbacksRef,
    withTransferGeneration: <T,>(fn: T) => fn, registerTransferCallback: (id: string, cb: () => Promise<void>) => { f.retryCallbacksRef.current.set(id, cb); },
    // App's own target checks and file batch, over the fixture's live
    // connection, which a test can switch while the picker is open.
    ...f.targetChecks,
    captureGuiTarget: async (check: () => void) => { check(); const epoch = await f.invoke('provider_gui_target_epoch'); check(); return epoch; },
    QueuedTargetChangedError: Error,
    uploadFile: f.uploadFile, notify: f.notify, checkOverwrite: f.checkOverwrite,
    humanLog: f.humanLog, isCancelledTransferError,
    joinRemotePath: (dir: string, name: string) => `${dir}/${name}`,
  };
  const run = new Function(...Object.keys(context), pickerCode)(...Object.values(context));
  await run();
}

describe('a failed file is seen by its queue row, the retry and the circuit breaker', () => {
  it('an upload in a session without a protocol retries a transient failure', async () => {
    const f = await upload({
      protocol: '',
      fail: (_command, call) => call === 1 ? 'Operation timed out' : undefined,
    });
    expect(attempts(f, 'upload_file')).toBe(2);
    expect(f.transferQueue.completeTransfer).toHaveBeenCalledTimes(1);
    expect(f.row('q0')).toMatchObject({ status: 'completed' });
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  it.each(['Operation timed out', 'Upload HTTP 503 Service Unavailable'])(
    'a provider upload answered with %s is not automatically replayed',
    async error => {
      const circuit = breaker();
      // The first write may already have committed. A second invocation would
      // succeed, but can create another revision of that very same file.
      const f = await upload({ breaker: circuit, fail: (_command, call) => call === 1 ? error : undefined });
      expect(attempts(f, UPLOAD)).toBe(1);
      expect(f.row('q0')).toMatchObject({ status: 'error', error });
      expect(f.transferQueue.completeTransfer).not.toHaveBeenCalled();
      expect(f.notify.error).toHaveBeenCalledTimes(1);
      expect(circuit.stateRef.current).toBe('closed'); // One failure, not three retries.

      // The owner can inspect the remote state and explicitly Retry the row.
      const [retry] = [...f.retryCallbacksRef.current.values()];
      f.transferQueue.retryItem('q0');
      await retry();
      expect(attempts(f, UPLOAD)).toBe(2);
      expect(f.row('q0')).toMatchObject({ status: 'completed' });
    },
  );

  it('three provider upload failures still reach the circuit breaker', async () => {
    const circuit = breaker();
    // One file per drop uses the single-file path, with a fresh run each time.
    // Preserve the breaker across runs to count independent failures here.
    circuit.reset = () => {};
    const f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: circuit, fail: () => 'Operation timed out' });
    for (let i = 0; i < 3; i++) await f.drop([local('up.txt')], false, REMOTE, 'local');
    expect(attempts(f, UPLOAD)).toBe(3);
    expect(circuit.stateRef.current).toBe('open');
    expect(f.notify.error).toHaveBeenCalledTimes(3);
  });

  it.each(['Operation timed out', 'Upload HTTP 503 Service Unavailable'])(
    'a picked provider upload answered with %s waits for explicit Retry',
    async error => {
      const circuit = breaker();
      const f = fixture({ breaker: circuit, fail: (_command, call) => call === 1 ? error : undefined });
      await uploadPicked(f, circuit);
      expect(attempts(f, UPLOAD)).toBe(1);
      expect(f.row('q0')).toMatchObject({ status: 'error', error });
      expect(f.notify.error).toHaveBeenCalledTimes(1);
      const [retry] = [...f.retryCallbacksRef.current.values()];
      f.transferQueue.retryItem('q0');
      await retry();
      expect(attempts(f, UPLOAD)).toBe(2);
      expect(f.row('q0')).toMatchObject({ status: 'completed' });
    },
  );

  it('a download that keeps timing out still retries twice', async () => {
    const circuit = breaker();
    const f = fixture({ folders: { [REMOTE]: [remote('a.txt')] }, breaker: circuit, fail: () => 'Operation timed out' });
    await f.drop([remote('a.txt')], true, SUB);
    expect(attempts(f, DOWNLOAD)).toBe(3);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'Operation timed out' });
    expect(circuit.stateRef.current).toBe('open');
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
      f = fixture({ folders: { [REMOTE]: [remote('a.txt')] }, breaker: breaker(), fail });
      await f.drop([remote('a.txt')], true, SUB);
      expect(attempts(f, DOWNLOAD)).toBe(2);
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
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser', cancelled: true });
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  // Two plain files go out as one batch; a file and a folder keep the loop.
  it('a cancel during a file also stops the folder after it in the same drop', async () => {
    let f: ReturnType<typeof fixture>;
    const fail = () => {
      f.batchCancelledRef.current = true;
      return 'Transfer cancelled';
    };
    const items = [local('up.txt'), folder(local('Sub'))];
    f = fixture({ folders: { [LOCAL]: items }, breaker: breaker(), fail });
    await f.drop(items, false, REMOTE, 'local');
    expect(attempts(f, UPLOAD)).toBe(1);
    expect(attempts(f, 'provider_upload_folder')).toBe(0);
    expect(f.row('q1')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser', cancelled: true });
  });

  it('a Stop during the wait before a download retry cancels the file', async () => {
    let f: ReturnType<typeof fixture>;
    const circuit = breaker();
    const delay = circuit.getRetryDelay;
    circuit.getRetryDelay = (attempt: number) => {
      f.batchCancelledRef.current = true;
      return delay(attempt);
    };
    f = fixture({ folders: { [REMOTE]: [remote('a.txt')] }, breaker: circuit,
      fail: (_command, call) => call === 1 ? 'Operation timed out' : undefined });
    await f.drop([remote('a.txt')], true, SUB);
    expect(attempts(f, DOWNLOAD)).toBe(1);
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser', cancelled: true });
    expect(f.transferQueue.completeTransfer).not.toHaveBeenCalled();
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

  it('Retry from the queue completes the row when the file lands', async () => {
    const f = await upload({ fail: (_command, call) => (call === 1 ? 'No such file or directory' : undefined) });
    const [retry] = [...f.retryCallbacksRef.current.values()];
    f.transferQueue.retryItem('q0');
    await retry();
    expect(attempts(f, UPLOAD)).toBe(2);
    expect(f.row('q0')).toMatchObject({ status: 'completed' });
    expect(f.notify.error).toHaveBeenCalledTimes(1);
  });

  it.each(['upload', 'download'] as const)('a Stop during a Retry cancels its file and shows no failure (%s)', async direction => {
    let f: ReturnType<typeof fixture>;
    const fail = (_command: string, call: number) => {
      if (call === 1) return 'No such file or directory';
      f.batchCancelledRef.current = true;
      return 'Transfer cancelled by user';
    };
    if (direction === 'upload') {
      f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: breaker(), fail });
      await f.drop([local('up.txt')], false, REMOTE, 'local');
    } else {
      f = fixture({ folders: { [REMOTE]: [remote('a.txt')] }, breaker: breaker(), fail });
      await f.drop([remote('a.txt')], true, SUB);
    }
    const [retry] = [...f.retryCallbacksRef.current.values()];
    f.transferQueue.retryItem('q0');
    await retry();
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser', cancelled: true });
    // The first attempt's error only.
    expect(f.notify.error).toHaveBeenCalledTimes(1);
  });

  // The flag of a stopped batch stays set until the next batch starts.
  it('a Retry after a stopped batch still shows its own error', async () => {
    let f: ReturnType<typeof fixture>;
    const fail = (_command: string, call: number) => {
      if (call === 1) f.batchCancelledRef.current = true;
      return call === 1 ? 'Transfer cancelled' : 'No such file or directory';
    };
    f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: breaker(), fail });
    await f.drop([local('up.txt')], false, REMOTE, 'local');
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser', cancelled: true });
    const [retry] = [...f.retryCallbacksRef.current.values()];
    f.transferQueue.retryItem('q0');
    await retry();
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'No such file or directory' });
    expect(f.notify.error).toHaveBeenCalledTimes(1);
  });

  it('Retry All Failed after a stopped batch retries the failed rows', async () => {
    const f = await upload({ fail: (_command, call) => (call === 1 ? 'No such file or directory' : undefined) });
    f.batchCancelledRef.current = true;
    await f.retryAllFailedItems();
    expect(attempts(f, UPLOAD)).toBe(2);
    expect(f.row('q0')).toMatchObject({ status: 'completed' });
  });

  it('a Stop during Retry All Failed retries no further row', async () => {
    let f: ReturnType<typeof fixture>;
    const fail = (_command: string, call: number) => {
      // The Stop lands during the first retry, which still fails.
      if (call === 3) f.stopCountRef.current += 1;
      return 'No such file or directory';
    };
    f = fixture({ folders: { [LOCAL]: [local('up.txt')] }, breaker: breaker(), fail });
    await f.drop([local('up.txt')], false, REMOTE, 'local');
    await f.drop([local('up.txt')], false, REMOTE, 'local');
    expect(attempts(f, UPLOAD)).toBe(2);
    await f.retryAllFailedItems();
    expect(attempts(f, UPLOAD)).toBe(3);
    expect(f.row('q1')).toMatchObject({ status: 'pending' });
  });
});

/**
 * A folder answers with counts, not an error: the row of a folder whose files
 * failed was completed. It now fails with the command's line; skipped files
 * are the overwrite setting at work and do not fail it.
 */
describe('a folder whose files failed fails its queue row', () => {
  const failedFolder = () => ({ completed: 3, failed: 2, message: 'Uploaded 3 files, 0 skipped, 2 errors' });

  it.each([['webdav', 'provider_upload_folder'], ['', 'upload_folder']] as const)(
    'an upload (%j): the row fails with the line, shown once, the folder sent once and not counted toward the pause',
    async (protocol, command) => {
      const circuit = breaker();
      const recordFailure = vi.spyOn(circuit, 'recordFailure');
      const sub = folder(local('Sub'));
      const f = fixture({ protocol, folders: { [LOCAL]: [sub] }, breaker: circuit, folderOutcome: failedFolder });
      await f.drop([sub], false, REMOTE, 'local');
      expect(attempts(f, command)).toBe(1);
      expect(f.row('q0')).toMatchObject({ status: 'error', error: 'Uploaded 3 files, 0 skipped, 2 errors' });
      expect(f.notify.error).toHaveBeenCalledTimes(1);
      expect(recordFailure).not.toHaveBeenCalled();
      // The activity line is not turned into a success.
      expect(f.humanLog.updateEntry.mock.calls.some(([, entry]) => entry.message === 'activity.upload_success')).toBe(false);
    },
  );

  it.each([['webdav', 'provider_download_folder'], ['', 'download_folder']] as const)('a download (%j): the row fails with the line', async (protocol, command) => {
    const circuit = breaker();
    const recordFailure = vi.spyOn(circuit, 'recordFailure');
    const pics = folder(remote('pics'));
    const f = fixture({
      protocol, folders: { [REMOTE]: [pics] }, breaker: circuit,
      folderOutcome: () => ({ completed: 1, failed: 1, message: 'Downloaded 1 files, 0 skipped, 1 errors' }),
    });
    await f.drop([pics], true, SUB);
    expect(attempts(f, command)).toBe(1);
    expect(recordFailure).not.toHaveBeenCalled();
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'Downloaded 1 files, 0 skipped, 1 errors' });
    expect(f.notify.error).toHaveBeenCalledTimes(1);
    expect(f.humanLog.updateEntry.mock.calls.some(([, entry]) => entry.status === 'success')).toBe(false);
  });

  it('a folder whose files were only skipped completes its row', async () => {
    const sub = folder(local('Sub'));
    const f = fixture({ folders: { [LOCAL]: [sub] }, breaker: breaker(), folderOutcome: () => ({ completed: 0, skipped: 4 }) });
    await f.drop([sub], false, REMOTE, 'local');
    expect(f.row('q0')).toMatchObject({ status: 'completed' });
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  it('a folder stopped by the user reads as cancelled, with no failure shown', async () => {
    let f: ReturnType<typeof fixture>;
    const folderOutcome = () => {
      f.batchCancelledRef.current = true;
      return { completed: 2, cancelled: true, message: 'Upload cancelled after 2 files' };
    };
    const sub = folder(local('Sub'));
    f = fixture({ folders: { [LOCAL]: [sub] }, breaker: breaker(), folderOutcome });
    await f.drop([sub], false, REMOTE, 'local');
    expect(f.row('q0')).toMatchObject({ status: 'error', error: 'transfer.cancelledByUser', cancelled: true });
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  // After a GitHub commit the folders go one by one, without a retry.
  it.each([
    ['whose upload is refused', { fail: (command: string) => (command === 'provider_upload_folder' ? 'Not connected' : undefined) }, 'Not connected'],
    ['whose files failed', { folderOutcome: failedFolder }, 'Uploaded 3 files, 0 skipped, 2 errors'],
  ] as const)('a folder after a GitHub commit %s fails its row', async (_what, options, error) => {
    const pics = folder(local('pics'));
    const f = fixture({ protocol: 'github', folders: { [LOCAL]: [local('up.txt'), pics] }, ...options });
    await f.drop([local('up.txt'), pics], false, INBOX, 'local');
    expect(f.commits()).toEqual([[`${INBOX}/up.txt`]]);
    expect(f.row('q1')).toMatchObject({ status: 'error', error });
    expect(f.notify.error).toHaveBeenCalledTimes(1);
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
    type Node = { type: string; start: number; end: number; [key: string]: unknown };
    const { program, errors } = parseSync('row-calls.tsx', source);
    expect(errors).toEqual([]);
    const found: { call: string; throws: boolean }[] = [];
    const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && 'type' in value;
    const isFunction = (node: Node) => /Function/.test(node.type) || node.type === 'ArrowFunctionExpression';
    const children = (node: Node) => Object.values(node).flatMap(value =>
      Array.isArray(value) ? value.filter(isNode) : isNode(value) ? [value] : []);
    const hasCompletionAfter = (block: Node, end: number): boolean => {
      const scan = (node: Node): boolean => {
        if (node !== block && (node.type === 'TryStatement' || isFunction(node))) return false;
        if (node.start >= end && node.type === 'CallExpression' && isNode(node.callee)) {
          const c = node.callee;
          if (c.type === 'MemberExpression' && isNode(c.object) && c.object.type === 'Identifier'
            && c.object.name === 'transferQueue' && isNode(c.property) && c.property.name === 'completeTransfer') return true;
        }
        return children(node).some(scan);
      };
      return scan(block);
    };
    const visit = (node: Node, parents: Node[] = []) => {
      if (node.type === 'AwaitExpression' && isNode(node.argument) && node.argument.type === 'CallExpression') {
        const call = node.argument;
        const callee = call.callee as Node;
        const name = callee.type === 'Identifier' ? callee.name
          : callee.type === 'MemberExpression' && isNode(callee.property) ? callee.property.name : null;
        if (name === 'downloadFile' || name === 'uploadFile') {
          let block: Node | undefined;
          for (const parent of [...parents].reverse()) {
            if (isFunction(parent)) break;
            if (parent.type === 'TryStatement') {
              if (isNode(parent.block) && parent.block.start <= node.start && node.end <= parent.block.end) block = parent.block;
              break;
            }
          }
          if (block && hasCompletionAfter(block, node.end)) {
            const argument = (call.arguments as Node[])[name === 'downloadFile' ? 6 : 7];
            found.push({ call: source.slice(node.start, node.end).replace(/\s+/g, ' '),
              throws: !!argument && ['Literal', 'StringLiteral'].includes(argument.type) && argument.value === 'throw' });
          }
        }
      }
      children(node).forEach(child => visit(child, [...parents, node]));
    };
    visit(program as unknown as Node);
    return found;
  }

  it('reads the actual error slot with optional ownership, nested commas and trailing arguments', () => {
    const source = `try { await fns.uploadFile(a, nested(b, c), false, 2, false, undefined, '/dir', 'throw', { profileId: 'A' }, ); transferQueue.completeTransfer(id); } catch (e) {}`;
    expect(rowCalls(source)).toEqual([{ call: source.slice(6, source.indexOf(';')).replace(/\s+/g, ' '), throws: true }]);
    expect(rowCalls(`try { await downloadFile(a, b, c, false, undefined, false, 'throw', owner); transferQueue.completeTransfer(id); } catch (e) {}`)[0].throws).toBe(true);
    for (const call of ["downloadFile(a, b)", "downloadFile(a, b, c, false, 2, false, 'notify', 'throw')", "uploadFile(a, b, false, 2, false, undefined, 'throw', 'notify')"]) {
      expect(rowCalls(`try { await ${call}; transferQueue.completeTransfer(id); } catch (e) {}`)[0].throws).toBe(false);
    }
    expect(rowCalls('if (await uploadFile(a)) ok += 1; try { x(); } catch (e) {}')).toEqual([]);
  });

  it('bounds completion to its own try block across optional catches, finally and nested functions', () => {
    const transfer = "await downloadFile(a,b,c,false,2,false,'throw')";
    for (const tail of ['catch {}', 'finally {}', 'catch (e) {} finally {}']) {
      expect(rowCalls(`try { ${transfer}; transferQueue.completeTransfer(id); } ${tail}`)).toHaveLength(1);
      expect(rowCalls(`try { ${transfer}; } ${tail} try { transferQueue.completeTransfer(other); } catch {}`)).toEqual([]);
    }
    expect(rowCalls(`try { ${transfer}; try { other(); } catch {} transferQueue.completeTransfer(id); } catch {}`)).toHaveLength(1);
    expect(rowCalls(`try { ${transfer}; try { transferQueue.completeTransfer(other); } catch {} } catch {}`)).toEqual([]);
    expect(rowCalls(`try { ${transfer}; const later = () => transferQueue.completeTransfer(other); } catch {}`)).toEqual([]);
    expect(rowCalls(`try { const later = async () => { ${transfer}; }; transferQueue.completeTransfer(other); } catch {}`)).toEqual([]);
    expect(rowCalls(`${transfer}; transferQueue.completeTransfer(id);`)).toEqual([]);
    expect(rowCalls(`try { x(); } catch { ${transfer}; } finally { transferQueue.completeTransfer(other); }`)).toEqual([]);
  });

  it('the three loops, the four Retry callbacks and the folders after a batch', () => {
    const calls = rowCalls(app);
    expect(calls.filter(c => !c.throws)).toEqual([]);
    // Nine calls: the Retry of a restored row has one for each direction.
    expect(calls).toHaveLength(9);
  });

  /** The catch block of each Retry callback, whole. */
  function retryCatches(source: string) {
    return [...source.matchAll(/transferQueue\.completeTransfer\(id\);\s*\} catch \(error\) \{/g)].map(match => {
      const start = match.index + match[0].length;
      let depth = 1;
      let end = start;
      for (; end < source.length && depth > 0; end++) {
        if (source[end] === '{') depth++;
        if (source[end] === '}') depth--;
      }
      return source.slice(start, end - 1);
    });
  }

  // The flag stays set after a stopped batch, so a Retry tells its own Stop
  // by the cancel error. The cases above run the drop's two; the dialog's
  // and the journal's are read here.
  it('each Retry callback takes a Stop during it for a cancel, and shows any other failure', () => {
    expect(retryCatches('transferQueue.completeTransfer(id);\n } catch (error) {\n if (a) { x(); } else { notify.error(a); }\n }\n y();'))
      .toEqual(['\n if (a) { x(); } else { notify.error(a); }\n ']);
    const catches = retryCatches(app);
    expect(catches).toHaveLength(4);
    expect(catches.filter(text => !text.includes('batchCancelledRef.current && isCancelledTransferError(String(error))'))).toEqual([]);
    expect(catches.filter(text => !/transferQueue\.failTransfer\(id, String\(error\)\);\s*notify\.error\(/.test(text))).toEqual([]);
  });

  // The cases run the drop's two loops; the dialog's is read here.
  it('each loop checks for a Stop after the wait before a retry', () => {
    const waits = [...app.matchAll(/await new Promise\(r => setTimeout\(r, circuitBreaker\.getRetryDelay\(attempt\)\)\);/g)]
      .map(({ index }) => app.slice(index, app.indexOf('transferQueue.retryItem(item.id);', index)));
    expect(waits).toHaveLength(3);
    const stops = /if \(batchCancelledRef\.current\) \{\s*transferQueue\.cancelTransfer\(item\.id, t\('transfer\.cancelledByUser'\)\);\s*break;/;
    expect(waits.filter(text => !stops.test(text))).toEqual([]);
  });

  // Retry All tells a Stop during it by the count, since the flag can be left
  // set: every place a Stop sets the flag counts it, in Stop's handler (which
  // counts first) or on the same line.
  it('every Stop that sets the flag is counted', () => {
    const start = app.indexOf('const cancelTransfer = async () => {');
    const end = app.indexOf('\n  };\n', start);
    expect(app.slice(start, end)).toMatch(/^const cancelTransfer = async \(\) => \{\n\s*stopCountRef\.current \+= 1;/);
    const uncounted = [...app.matchAll(/batchCancelledRef\.current = true/g)].filter(({ index }) => {
      if (index > start && index < end) return false;
      const line = app.slice(app.lastIndexOf('\n', index), app.indexOf('\n', index));
      return !line.includes('stopCountRef.current += 1');
    });
    expect(uncounted).toEqual([]);
  });
});

/**
 * The picker stays open as long as the user wants: a switch to another server
 * meanwhile left the upload to run on it, because the batch took its target
 * after the picker. The target is the one taken before it opened.
 */
describe('a switch to another server while the file picker is open', () => {
  it.each([
    ['one file', [`${LOCAL}/up.txt`]],
    ['two files', [`${LOCAL}/up.txt`, `${LOCAL}/b.txt`]],
  ])('uploads nothing to the server switched to, with %s', async (_label, picked) => {
    const f = fixture({ folders: { [LOCAL]: [local('up.txt'), local('b.txt')] } });
    await uploadPicked(f, breaker(), picked, () => f.switchServer());
    expect(f.written()).toEqual([]);
  });

  it('uploads when the server stays the same', async () => {
    const f = fixture({ folders: { [LOCAL]: [local('up.txt'), local('b.txt')] } });
    await uploadPicked(f, breaker(), [`${LOCAL}/up.txt`, `${LOCAL}/b.txt`]);
    expect(f.written()).toEqual([`${REMOTE}/up.txt`, `${REMOTE}/b.txt`]);
  });
});

it('does not dispatch multiple picker files after Stop while the picker is open', async () => {
  const f = fixture({ folders: { [LOCAL]: [local('up.txt'), local('b.txt')] } });
  await uploadPicked(f, breaker(), [`${LOCAL}/up.txt`, `${LOCAL}/b.txt`], () => {
    f.stopCountRef.current++;
    f.batchCancelledRef.current = true;
  });
  expect(f.written()).toEqual([]);
  expect(f.transferQueue.addItem).not.toHaveBeenCalled();
});

it.each(['upload', 'download'] as const)('creates no pending %s rows after Stop during destination listing', async direction => {
  const entry = direction === 'upload' ? folder(local('dir')) : folder(remote('dir'));
  const destination = direction === 'upload' ? INBOX : SUB;
  const f = fixture({ folders: { [LOCAL]: [folder(local('dir'))], [REMOTE]: [folder(remote('dir'))], [destination]: [] } });
  let finish!: () => void;
  let listing = false;
  const held = new Promise<void>(resolve => { finish = resolve; });
  const original = f.invoke.getMockImplementation()!;
  f.invoke.mockImplementation(async (cmd: string, args?: Record<string, any>) => {
    if (cmd === (direction === 'upload' ? 'provider_list_files' : 'get_local_files') && args?.path === destination) {
      listing = true;
      await held;
    }
    return original(cmd, args);
  });
  const pending = f.drop([entry], direction === 'download', destination, 'local');
  await vi.waitFor(() => expect(listing).toBe(true));
  f.stopCountRef.current++;
  f.batchCancelledRef.current = true;
  finish();
  await pending;
  expect(f.transferQueue.addItem).not.toHaveBeenCalled();
  expect(f.written()).toEqual([]);
});
