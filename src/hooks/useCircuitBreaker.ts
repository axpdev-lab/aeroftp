/**
 * Circuit Breaker Hook for Batch Transfers
 *
 * Monitors consecutive transfer errors and trips the circuit when threshold is reached.
 * Supports three states:
 *   - closed: normal operation
 *   - open: circuit tripped (fatal error or consecutive failures)
 *   - half_open: attempting reconnection
 */

import { useRef, useCallback } from 'react';
import { SyncErrorKind } from '../types';
import { classifyErrorFast, FATAL_ERROR_KINDS, PER_FILE_ERROR_KINDS, RECONNECT_ERROR_KINDS } from '../utils/transferErrorClassifier';

export type CircuitBreakerState = 'closed' | 'open' | 'half_open';
export type PauseReason = 'consecutive_errors' | 'fatal_error' | 'reconnecting';

export interface CircuitBreakerConfig {
  /** Max consecutive errors before tripping (default: 3) */
  maxConsecutiveErrors: number;
  /** Max retries per file for retryable errors (default: 2) */
  maxRetriesPerFile: number;
  /** Base delay between retries in ms (default: 1000) */
  baseRetryDelay: number;
  /** Max delay cap in ms (default: 10000) */
  maxRetryDelay: number;
  /** Backoff multiplier (default: 2) */
  backoffMultiplier: number;
}

export const DEFAULT_CONFIG: CircuitBreakerConfig = {
  maxConsecutiveErrors: 3,
  maxRetriesPerFile: 2,
  baseRetryDelay: 1000,
  maxRetryDelay: 10000,
  backoffMultiplier: 2,
};

export interface RecordFailureResult {
  shouldPause: boolean;
  isFatal: boolean;
  errorKind: SyncErrorKind;
  retryable: boolean;
}

export function useCircuitBreaker(config: CircuitBreakerConfig = DEFAULT_CONFIG) {
  const stateRef = useRef<CircuitBreakerState>('closed');
  const consecutiveErrorsRef = useRef(0);
  const pauseReasonRef = useRef<PauseReason | null>(null);
  const tripErrorKindRef = useRef<SyncErrorKind | null>(null);

  /** Record a successful transfer — resets consecutive error counter */
  const recordSuccess = useCallback(() => {
    consecutiveErrorsRef.current = 0;
    stateRef.current = 'closed';
    pauseReasonRef.current = null;
    tripErrorKindRef.current = null;
  }, []);

  /** Record a failed transfer — classifies error and decides whether to trip */
  const recordFailure = useCallback((errorMessage: string): RecordFailureResult => {
    const { kind, retryable } = classifyErrorFast(errorMessage);

    // Fatal errors: immediate trip, no retry possible
    if (FATAL_ERROR_KINDS.has(kind)) {
      stateRef.current = 'open';
      pauseReasonRef.current = 'fatal_error';
      tripErrorKindRef.current = kind;
      consecutiveErrorsRef.current = 0;
      return { shouldPause: true, isFatal: true, errorKind: kind, retryable: false };
    }

    // Per-file errors (path not found, permission denied): don't count toward consecutive
    if (PER_FILE_ERROR_KINDS.has(kind)) {
      return { shouldPause: false, isFatal: false, errorKind: kind, retryable };
    }

    // Retryable errors: increment consecutive counter
    consecutiveErrorsRef.current++;

    if (consecutiveErrorsRef.current >= config.maxConsecutiveErrors) {
      stateRef.current = 'open';
      pauseReasonRef.current = RECONNECT_ERROR_KINDS.has(kind) ? 'reconnecting' : 'consecutive_errors';
      tripErrorKindRef.current = kind;
      return { shouldPause: true, isFatal: false, errorKind: kind, retryable };
    }

    return { shouldPause: false, isFatal: false, errorKind: kind, retryable };
  }, [config.maxConsecutiveErrors]);

  /** Calculate exponential backoff delay for a given attempt (1-indexed) */
  const getRetryDelay = useCallback((attempt: number): number => {
    const delay = config.baseRetryDelay * Math.pow(config.backoffMultiplier, Math.max(0, attempt - 1));
    return Math.min(delay, config.maxRetryDelay);
  }, [config.baseRetryDelay, config.backoffMultiplier, config.maxRetryDelay]);

  /** Whether to retry a file based on attempt count and error kind */
  const shouldRetryFile = useCallback((attempt: number, errorKind: SyncErrorKind): boolean => {
    if (FATAL_ERROR_KINDS.has(errorKind)) return false;
    if (PER_FILE_ERROR_KINDS.has(errorKind)) return false;
    return attempt < config.maxRetriesPerFile;
  }, [config.maxRetriesPerFile]);

  /** Reset circuit breaker for a new batch or after successful resume */
  const reset = useCallback(() => {
    stateRef.current = 'closed';
    consecutiveErrorsRef.current = 0;
    pauseReasonRef.current = null;
    tripErrorKindRef.current = null;
  }, []);

  /** Transition to half_open state while attempting reconnection */
  const markReconnecting = useCallback(() => {
    stateRef.current = 'half_open';
    pauseReasonRef.current = 'reconnecting';
  }, []);

  /** Reconnection succeeded — close circuit and reset counters */
  const markReconnected = useCallback(() => {
    stateRef.current = 'closed';
    consecutiveErrorsRef.current = 0;
    pauseReasonRef.current = null;
    tripErrorKindRef.current = null;
  }, []);

  /** Reconnection failed — open circuit as fatal */
  const markReconnectFailed = useCallback(() => {
    stateRef.current = 'open';
    pauseReasonRef.current = 'fatal_error';
  }, []);

  return {
    config,
    stateRef,
    pauseReasonRef,
    tripErrorKindRef,
    recordSuccess,
    recordFailure,
    getRetryDelay,
    shouldRetryFile,
    reset,
    markReconnecting,
    markReconnected,
    markReconnectFailed,
  };
}
