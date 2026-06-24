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
  if (error instanceof Error) return error.message.includes(CONNECT_CANCELLED_MARKER);
  return String(error).includes(CONNECT_CANCELLED_MARKER);
}
