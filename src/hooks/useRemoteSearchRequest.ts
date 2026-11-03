// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { useCallback, useLayoutEffect, useRef, useState } from 'react';

/** Separate ownership of search results from native calls still holding the provider lock. */
export function useRemoteSearchRequest(context: string) {
  const generation = useRef(0);
  const mounted = useRef(false);
  const [pendingCount, setPendingCount] = useState(0);
  const cancel = useCallback(() => { generation.current += 1; }, []);
  useLayoutEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  useLayoutEffect(() => { cancel(); return cancel; }, [context, cancel]);
  const begin = useCallback(() => {
    const request = ++generation.current;
    setPendingCount((count) => count + 1);
    let finished = false;
    return {
      isCurrent: () => mounted.current && generation.current === request,
      finish: () => {
        if (finished) return;
        finished = true;
        if (mounted.current) setPendingCount((count) => count - 1);
      },
    };
  }, []);
  return { begin, cancel, pendingCount };
}
