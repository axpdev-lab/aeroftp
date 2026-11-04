// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it } from 'vitest';
import { fixture, LOCAL, REMOTE } from './appTransfers.testkit';

it.each(['webdav', 'opendrive', 'fourshared'] as const)('passes the saved scope to every restored transfer/privacy RPC on %s', async protocol => {
  const f = fixture({ protocol, policy: 'overwrite' });
  const owner = { profileId: 'A', generation: 17, cryptScope: REMOTE, requireOverlay: true };
  await f.downloadFile(`${REMOTE}/file`, 'file', LOCAL, false, 40, false, 'throw', owner);
  await f.downloadFile(`${REMOTE}/dir`, 'dir', LOCAL, true, undefined, false, 'throw', owner);
  await f.uploadFile(`${LOCAL}/file`, 'file', false, 40, false, undefined, REMOTE, 'throw', owner);
  await f.uploadFile(`${LOCAL}/dir`, 'dir', true, undefined, false, undefined, REMOTE, 'throw', owner);
  const calls = f.invoke.mock.calls.filter(([command]) => /provider_(upload|download)_(file|folder)$|set_path_privacy$/.test(command));
  expect(calls.length).toBe(protocol === 'webdav' ? 4 : 6);
  for (const [,args] of calls) expect(args).toMatchObject({ expectedProfileId: 'A', expectedGeneration: 17, expectedCryptScope: REMOTE, requireCryptOverlay: true });
});
