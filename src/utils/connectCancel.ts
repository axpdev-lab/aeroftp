// W3.1 (#270.5 / #360): shared marker for a user-cancelled connect attempt.
// The backend returns `CONNECT_CANCELLED` when `cancel_connection` aborts an
// in-progress connection (Esc / "still connecting" Cancel). The connect flows
// match on it to surface a calm "connection cancelled" toast instead of a
// "connection failed" error, and to skip the saved-server connect-failure
// marker (a deliberate cancel is not a failure).
//
// Lives in its own module so both App.tsx (the connect orchestrator) and the
// My Servers card path (MyServersPanel, which drives OAuth connects directly)
// share one definition instead of duplicating the literal.
export const CONNECT_CANCELLED_MARKER = 'CONNECT_CANCELLED';

export function isConnectCancelledError(error: unknown): boolean {
  if (error == null) return false;
  if (error instanceof Error) return error.message === CONNECT_CANCELLED_MARKER;
  return String(error) === CONNECT_CANCELLED_MARKER;
}

// A connect that a newer connect superseded while it ran: it stops where it
// is, before any teardown, replacement or landing, and touches nothing of the
// connect that owns the workspace now (no toast, no session restored).
export const CONNECT_SUPERSEDED_MARKER = 'CONNECT_SUPERSEDED';

export function isConnectSupersededError(error: unknown): boolean {
  if (error == null) return false;
  if (error instanceof Error) return error.message === CONNECT_SUPERSEDED_MARKER;
  return String(error) === CONNECT_SUPERSEDED_MARKER;
}

// IPC panic safety net (v4.1.3): a panicking Tauri command never answers its
// invoke, so the connect promise would hang forever. `cancellableConnect` races
// the connect against a hard-timeout deadline that rejects with this marker.
// Unlike a user cancel, a hard timeout is a real failure: the connect flow's
// catch should run its failure path (surface an error, mark the saved server),
// not the calm "cancelled" path, so this is kept distinct from the cancel marker.
export const CONNECT_HARD_TIMEOUT_MARKER = 'CONNECT_HARD_TIMEOUT';

export function isConnectHardTimeoutError(error: unknown): boolean {
  if (error == null) return false;
  if (error instanceof Error) return error.message.includes(CONNECT_HARD_TIMEOUT_MARKER);
  return String(error).includes(CONNECT_HARD_TIMEOUT_MARKER);
}

// A connect refused before its command was sent: an older connect a newer
// phase superseded is still running, and its cancel never reached the backend
// (both deliveries failed), so it could still install over this one. A
// failure, not a cancel (the failure toast and the saved-server marker). Its
// message is the text shown to the user, also through `String(error)`.
export class ConnectEarlierNotStoppedError extends Error {
  override toString(): string {
    return this.message;
  }
}

export function isConnectEarlierNotStoppedError(error: unknown): boolean {
  return error instanceof ConnectEarlierNotStoppedError;
}
