// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Runs `read` and says whether it was still the latest read on `seq` when it
 * settled. A polled list is read again (an event, window focus, the poll, an
 * action) while an earlier read can still be pending, and the earlier one can
 * settle last: committing it would replace newer data with older. Commit
 * `value` only when `latest` is true. `seq` is one `useRef(0)` per list.
 */
export async function readLatest<T>(
  seq: { current: number },
  read: () => Promise<T>,
): Promise<{ value: T; latest: boolean }> {
  const id = ++seq.current;
  const value = await read();
  return { value, latest: id === seq.current };
}
