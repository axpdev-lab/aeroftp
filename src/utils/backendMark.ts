// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** The backend's replacement and confirmation counts at one moment
 *  (App's `backendMark`). */
export interface BackendMark {
  replacements: number;
  confirmations: number;
}

/** Where a connect's replacement left the backend: the mark its end produced,
 *  filled synchronously by that end and carried back by the connect wrappers
 *  (`cancellableConnect`, `runConnect`), so a replacement another connect ends
 *  while the result travels back is not counted as this connect's. */
export interface ConnectReceipt {
  mark?: BackendMark;
  /** The connect phase the connect belongs to, when it began before its
   *  command was sent (a preflight in between): one a newer phase overtook
   *  meanwhile is not sent (`trackedConnect`). */
  phase?: number;
}
