/**
 * Transfer Error Classifier
 *
 * Synchronous error classification mirroring sync.rs:679 classify_sync_error().
 * Used by the circuit breaker in batch transfer loops for zero-latency classification.
 */

import { SyncErrorKind } from '../types';

/**
 * Fast synchronous classification for common error patterns.
 * Pattern matching order matches sync.rs:682-717 for consistency.
 */
export function classifyErrorFast(rawError: string): { kind: SyncErrorKind; retryable: boolean } {
  const lower = rawError.toLowerCase();

  // Quota / storage full (FTP 552) — NOT retryable
  if (lower.includes('quota') || lower.includes('storage full') || lower.includes('insufficient storage') || lower.includes('552 ')) {
    return { kind: 'quota_exceeded', retryable: false };
  }
  // Authentication (FTP 530) — NOT retryable
  if (lower.includes('auth') || lower.includes('login') || lower.includes('credential') || lower.includes('401 ') || lower.includes('530 ')) {
    return { kind: 'auth', retryable: false };
  }
  // Disk full / I/O — NOT retryable
  if (lower.includes('disk full') || lower.includes('no space') || lower.includes('i/o error') || lower.includes('broken pipe')) {
    return { kind: 'disk_error', retryable: false };
  }
  // Permission denied (FTP 550, HTTP 403) — NOT retryable
  // Note: FTP 550 is ambiguous — exclude "not found" / "no such" so they classify as path_not_found below
  if (lower.includes('permission denied') || lower.includes('access denied') || lower.includes('403 ')
    || (lower.includes('550 ') && !lower.includes('not found') && !lower.includes('no such'))) {
    return { kind: 'permission_denied', retryable: false };
  }
  // Timeout — retryable
  if (lower.includes('timeout') || lower.includes('timed out')) {
    return { kind: 'timeout', retryable: true };
  }
  // Rate limit (HTTP 429) — retryable
  if (lower.includes('rate limit') || lower.includes('too many requests') || lower.includes('429')) {
    return { kind: 'rate_limit', retryable: true };
  }
  // File locked — retryable
  if (lower.includes('locked') || lower.includes('in use')) {
    return { kind: 'file_locked', retryable: true };
  }
  // Network / connection — retryable
  if (lower.includes('connection') || lower.includes('network') || lower.includes('dns')
    || lower.includes('refused') || lower.includes('reset') || lower.includes('eof')
    || lower.includes('data connection') || lower.includes('not connected')) {
    return { kind: 'network', retryable: true };
  }
  // Path not found (FTP 550, HTTP 404) — NOT retryable but not fatal
  if (lower.includes('not found') || lower.includes('no such file') || lower.includes('404 ')) {
    return { kind: 'path_not_found', retryable: false };
  }
  // Unknown — default to retryable (conservative)
  return { kind: 'unknown', retryable: true };
}

/** Error kinds that should immediately stop the entire batch — no retry possible */
export const FATAL_ERROR_KINDS: Set<SyncErrorKind> = new Set([
  'quota_exceeded',
  'auth',
  'disk_error',
]);

/** Error kinds that indicate connection loss — should trigger reconnect attempt */
export const RECONNECT_ERROR_KINDS: Set<SyncErrorKind> = new Set([
  'network',
  'timeout',
]);

/** Error kinds that are per-file issues — do NOT count toward consecutive errors */
export const PER_FILE_ERROR_KINDS: Set<SyncErrorKind> = new Set([
  'path_not_found',
  'permission_denied',
  'file_locked',
]);

/**
 * Returns the i18n key for a human-readable error kind label.
 */
export function getErrorKindI18nKey(kind: SyncErrorKind): string {
  return `transfer.errorKind.${kind}`;
}
