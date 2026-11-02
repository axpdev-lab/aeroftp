// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { useCallback, useLayoutEffect, useRef } from 'react';

/** Owns the results, errors and spinner of one remote search on one panel. */
export function useRemoteSearchRequest(context: string) {
  const generation = useRef(0);
  const cancel = useCallback(() => { generation.current += 1; }, []);
  useLayoutEffect(() => { cancel(); return cancel; }, [context, cancel]);
  const begin = useCallback(() => {
    const request = ++generation.current;
    return () => generation.current === request;
  }, []);
  return { begin, cancel };
}
