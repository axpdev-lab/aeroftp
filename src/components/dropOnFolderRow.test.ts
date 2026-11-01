// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { INBOX, LOCAL, PANEL2, REMOTE, SUB, fixture, folder, local, remote } from './appTransfers.testkit';

/**
 * A drop on the other panel writes into the folder it was dropped on.
 *
 * The drag hook hands the drop its folder: the folder row under the pointer,
 * or the folder the panel shows when the drop lands on its empty space. The
 * drop ignored it, so files dropped on a folder row went into the folder the
 * other panel shows, and remote files dropped on panel 2 went into panel 1's
 * folder. A folder the panel does not list has to be listed for the
 * overwrite questions, and a listing that fails stops the transfer: it never
 * passes for an empty folder (#1049).
 *
 * This runs App's own closures, from the drop to the IPC call that writes,
 * with the IPC mocked: the paths checked are the ones the backend would be
 * asked to write.
 */

describe('a drop on the other panel writes into the folder it was dropped on', () => {
  // One file keeps the single-file path, two go out as one batch.
  describe.each([['one file', ['up.txt']], ['two files', ['up.txt', 'b.txt']]] as const)('%s', (_label, names) => {
    it('local files dropped on a remote folder row are uploaded into that folder', async () => {
      const f = fixture({ folders: { [LOCAL]: names.map(n => local(n)) } });
      await f.drop(names.map(n => local(n)), false, INBOX, 'local');
      expect(f.written()).toEqual(names.map(n => `${INBOX}/${n}`));
      expect(f.notify.error).not.toHaveBeenCalled();
    });

    it('remote files dropped on a local folder row are downloaded into that folder', async () => {
      const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
      await f.drop(names.map(n => remote(n)), true, SUB);
      expect(f.written()).toEqual(names.map(n => `${SUB}/${n}`));
      expect(f.refreshLocalPanelForPath).toHaveBeenCalledWith(SUB);
      expect(f.notify.error).not.toHaveBeenCalled();
    });

    it('remote files dropped on panel 2 are downloaded into the folder panel 2 shows', async () => {
      const f = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
      await f.drop(names.map(n => remote(n)), true, PANEL2);
      expect(f.written()).toEqual(names.map(n => `${PANEL2}/${n}`));
      expect(f.refreshLocalPanelForPath).toHaveBeenCalledWith(PANEL2);
    });

    it('a drop on the panel\'s own folder still writes there', async () => {
      const up = fixture({ folders: { [LOCAL]: names.map(n => local(n)) } });
      await up.drop(names.map(n => local(n)), false, REMOTE, 'local');
      expect(up.written()).toEqual(names.map(n => `${REMOTE}/${n}`));
      const down = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) } });
      await down.drop(names.map(n => remote(n)), true, LOCAL);
      expect(down.written()).toEqual(names.map(n => `${LOCAL}/${n}`));
    });

    it('asks about what the folder dropped on holds, not what the panel lists', async () => {
      // The remote panel lists none of the names; Inbox holds them all: with
      // "skip", nothing is uploaded.
      const up = fixture({ folders: { [LOCAL]: names.map(n => local(n)), [INBOX]: names.map(n => remote(n, INBOX)) } });
      await up.drop(names.map(n => local(n)), false, INBOX, 'local');
      expect(up.written()).toEqual([]);
      expect(up.checkOverwrite).toHaveBeenCalledTimes(names.length);
      // The local panel lists them all; Sub holds none: everything is downloaded.
      const down = fixture({ folders: { [REMOTE]: names.map(n => remote(n)), [LOCAL]: names.map(n => local(n)) } });
      await down.drop(names.map(n => remote(n)), true, SUB);
      expect(down.written()).toEqual(names.map(n => `${SUB}/${n}`));
    });

    it('a folder dropped on that cannot be listed stops the transfer before a queue row', async () => {
      const up = fixture({ folders: { [LOCAL]: names.map(n => local(n)) }, unlistable: [INBOX] });
      await up.drop(names.map(n => local(n)), false, INBOX, 'local');
      const down = fixture({ folders: { [REMOTE]: names.map(n => remote(n)) }, unlistable: [SUB] });
      await down.drop(names.map(n => remote(n)), true, SUB);
      for (const f of [up, down]) {
        expect(f.written()).toEqual([]);
        expect(f.transferQueue.addItem).not.toHaveBeenCalled();
        expect(f.notify.error).toHaveBeenCalled();
      }
    });
  });

  // A folder goes out on its own, after the folder question: the question
  // reads what the folder dropped on holds.
  it('a folder dropped on a folder row is asked about and written there', async () => {
    const pics = folder(local('pics'));
    const up = fixture({ folders: { [LOCAL]: [pics], [INBOX]: [folder(remote('pics', INBOX))] } });
    await up.drop([pics], false, INBOX, 'local');
    expect(up.checkFolderOverwrite.mock.calls[0][3]).toEqual([folder(remote('pics', INBOX))]);
    expect(up.written()).toEqual([`${INBOX}/pics`]);
    expect(up.notify.error).not.toHaveBeenCalled();
    const rpics = folder(remote('pics'));
    const down = fixture({ folders: { [REMOTE]: [rpics], [SUB]: [folder(local('pics', SUB))] } });
    await down.drop([rpics], true, SUB);
    expect(down.checkFolderOverwrite.mock.calls[0][3]).toEqual([folder(local('pics', SUB))]);
    expect(down.written()).toEqual([`${SUB}/pics`]);
    expect(down.notify.error).not.toHaveBeenCalled();
  });

  // A repository upload is one commit of the files; a folder goes after it.
  it('GitHub: files dropped on a folder row are committed into that folder, a folder with them', async () => {
    const pics = folder(local('pics'));
    const f = fixture({ protocol: 'github', folders: { [LOCAL]: [local('up.txt'), pics] } });
    await f.drop([local('up.txt'), pics], false, INBOX, 'local');
    expect(f.requestGitHubBatchCommitMessage.mock.calls[0][0]).toEqual([
      { local: `${LOCAL}/up.txt`, remote: `${INBOX}/up.txt` },
      { local: `${LOCAL}/pics`, remote: `${INBOX}/pics` },
    ]);
    expect(f.written()).toEqual([`${INBOX}/up.txt`, `${INBOX}/pics`]);
    expect(f.notify.error).not.toHaveBeenCalled();
  });

  // The folder question decides: a folder it skips is not written, in either
  // direction.
  it('a folder the folder question skips is not written', async () => {
    const pics = folder(local('pics'));
    const up = fixture({ folderAction: 'skip', folders: { [LOCAL]: [pics], [INBOX]: [folder(remote('pics', INBOX))] } });
    await up.drop([pics], false, INBOX, 'local');
    const rpics = folder(remote('pics'));
    const down = fixture({ folderAction: 'skip', folders: { [REMOTE]: [rpics], [SUB]: [folder(local('pics', SUB))] } });
    await down.drop([rpics], true, SUB);
    for (const f of [up, down]) {
      expect(f.checkFolderOverwrite).toHaveBeenCalledTimes(1);
      expect(f.written()).toEqual([]);
      expect(f.transferQueue.completeTransfer).toHaveBeenCalledWith('q0');
    }
  });

  // The commit writes every file it names at once: it takes only what the
  // overwrite settings let through, as the per-file path does.
  describe('GitHub: the commit follows the overwrite settings', () => {
    const files = () => [local('up.txt'), local('new.txt')];
    it('a file the settings skip stays out of the commit', async () => {
      const f = fixture({ protocol: 'github', policy: 'skip', folders: { [LOCAL]: files(), [INBOX]: [remote('up.txt', INBOX)] } });
      await f.drop(files(), false, INBOX, 'local');
      expect(f.checkOverwrite).toHaveBeenCalledTimes(2);
      expect(f.commits()).toEqual([[`${INBOX}/new.txt`]]);
      expect(f.notify.info).toHaveBeenCalledWith('toast.fileSkipped');
    });

    it('a renamed file is committed under its new name', async () => {
      const f = fixture({ protocol: 'github', policy: 'rename', folders: { [LOCAL]: files(), [INBOX]: [remote('up.txt', INBOX)] } });
      await f.drop(files(), false, INBOX, 'local');
      expect(f.commits()).toEqual([[`${INBOX}/up (1).txt`, `${INBOX}/new.txt`]]);
    });

    it('with every file skipped there is no commit', async () => {
      const f = fixture({ protocol: 'github', policy: 'skip', folders: { [LOCAL]: files(), [INBOX]: files().map(l => remote(l.name, INBOX)) } });
      await f.drop(files(), false, INBOX, 'local');
      expect(f.commits()).toEqual([]);
      expect(f.written()).toEqual([]);
    });

    it('a folder the folder question skips is not written after the commit', async () => {
      const pics = folder(local('pics'));
      const f = fixture({ protocol: 'github', folderAction: 'skip', folders: { [LOCAL]: [local('up.txt'), pics], [INBOX]: [folder(remote('pics', INBOX))] } });
      await f.drop([local('up.txt'), pics], false, INBOX, 'local');
      expect(f.checkFolderOverwrite).toHaveBeenCalledTimes(1);
      expect(f.written()).toEqual([`${INBOX}/up.txt`]);
    });
  });

  // The files go out as one batch first; the folder after them needs the
  // folder dropped on listed again. When that listing fails, the batch has
  // already written: the run ends as the batch alone would.
  it('a listing that fails after the file batch still clears the selection and reloads', async () => {
    const pics = folder(local('pics'));
    const upFiles = [local('a.txt'), local('b.txt'), pics];
    const up = fixture({ folders: { [LOCAL]: upFiles }, listingFailsFrom: { [INBOX]: 2 } });
    await up.drop(upFiles, false, INBOX, 'local');
    expect(up.written()).toEqual([`${INBOX}/a.txt`, `${INBOX}/b.txt`]);
    expect(up.notify.error).toHaveBeenCalledWith('toast.uploadFailed', expect.any(String));
    expect(up.panels.local.setSelection).toHaveBeenCalledWith(new Set());
    expect(up.loadRemoteFiles).toHaveBeenCalled();

    const rpics = folder(remote('pics'));
    const downFiles = [remote('a.txt'), remote('b.txt'), rpics];
    const down = fixture({ folders: { [REMOTE]: downFiles }, listingFailsFrom: { [SUB]: 2 } });
    await down.drop(downFiles, true, SUB);
    expect(down.written()).toEqual([`${SUB}/a.txt`, `${SUB}/b.txt`]);
    expect(down.notify.error).toHaveBeenCalledWith('toast.downloadFailed', expect.any(String));
    expect(down.setSelectedRemoteFiles).toHaveBeenCalledWith(new Set());
    expect(down.refreshLocalPanelForPath).toHaveBeenCalledWith(SUB);
  });

  // Retry runs the callback the queue row was given, later, when the panel
  // may show another folder: it writes where the drop was queued for.
  it('Retry of an upload writes into the folder it was dropped on', async () => {
    const f = fixture({ folders: { [LOCAL]: [local('up.txt')] } });
    await f.drop([local('up.txt')], false, INBOX, 'local');
    const [retry] = [...f.retryCallbacksRef.current.values()];
    await retry();
    expect(f.written()).toEqual([`${INBOX}/up.txt`, `${INBOX}/up.txt`]);
  });

  it('Retry of a download writes into the folder it was dropped on', async () => {
    const f = fixture({ folders: { [REMOTE]: [remote('a.txt')] } });
    await f.drop([remote('a.txt')], true, SUB);
    const [retry] = [...f.retryCallbacksRef.current.values()];
    await retry();
    expect(f.written()).toEqual([`${SUB}/a.txt`, `${SUB}/a.txt`]);
  });

  it('a nested row of panel 2 is uploaded from panel 2, into the folder dropped on', async () => {
    const nested = { ...local('deep.txt', `${PANEL2}/x`), name: 'x/deep.txt' };
    const f = fixture({ folders: { [PANEL2]: [nested] } });
    await f.drop([{ name: 'deep.txt', path: nested.path }], false, INBOX, 'local2');
    expect(f.written()).toEqual([`${INBOX}/deep.txt`]);
    const upload = f.invoke.mock.calls.find(([command]) => command === 'provider_upload_file')!;
    expect(upload[1]!.localPath).toBe(nested.path);
  });
});
