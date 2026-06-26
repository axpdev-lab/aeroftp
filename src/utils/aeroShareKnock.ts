// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The PREDEFINED knock catalog: a knock is a tiny ping carrying an opaque short
 * CODE (no free text - this is a notification, not a chat). The backend relays
 * the code; this catalog maps it to a localized label and, for a question knock,
 * to the set of one-tap REPLY codes, enabling a bounded predefined
 * question/answer exchange (e.g. "Are you online?" -> "Yes, I'm here" / "Not now").
 *
 * To extend the vocabulary, add a code here + its i18n key under
 * `aeroShare.knock.msg.*` in en.json (and translate). No emoji, ever.
 */

export interface KnockDef {
  /** Stable wire code (sent to/from the peer). */
  code: string;
  /** i18n key for the human label of this message. */
  labelKey: string;
  /** Reply codes offered as one-tap answers when THIS knock is received. */
  replies?: string[];
  /** Shown in the "send a ping" menu as an initiating message (vs reply-only). */
  sendable?: boolean;
}

export const KNOCKS: Record<string, KnockDef> = {
  knock: { code: 'knock', labelKey: 'aeroShare.knock.msg.knock', sendable: true },
  online_q: {
    code: 'online_q',
    labelKey: 'aeroShare.knock.msg.online_q',
    sendable: true,
    replies: ['online_yes', 'online_no'],
  },
  ready_q: {
    code: 'ready_q',
    labelKey: 'aeroShare.knock.msg.ready_q',
    sendable: true,
    replies: ['ready_yes', 'ready_no'],
  },
  check_inbox: {
    code: 'check_inbox',
    labelKey: 'aeroShare.knock.msg.check_inbox',
    sendable: true,
    replies: ['thanks'],
  },
  please_accept: { code: 'please_accept', labelKey: 'aeroShare.knock.msg.please_accept', sendable: true },
  one_moment: { code: 'one_moment', labelKey: 'aeroShare.knock.msg.one_moment', sendable: true },
  resend: { code: 'resend', labelKey: 'aeroShare.knock.msg.resend', sendable: true },
  // Reply-only codes (offered as quick answers, not in the initiating menu).
  online_yes: { code: 'online_yes', labelKey: 'aeroShare.knock.msg.online_yes' },
  online_no: { code: 'online_no', labelKey: 'aeroShare.knock.msg.online_no' },
  ready_yes: { code: 'ready_yes', labelKey: 'aeroShare.knock.msg.ready_yes' },
  ready_no: { code: 'ready_no', labelKey: 'aeroShare.knock.msg.ready_no' },
  thanks: { code: 'thanks', labelKey: 'aeroShare.knock.msg.thanks' },
};

/** Codes offered in the "send a ping" launcher, in display order. */
export const SENDABLE_KNOCK_CODES: string[] = [
  'knock',
  'online_q',
  'ready_q',
  'check_inbox',
  'please_accept',
  'one_moment',
  'resend',
];

/** Resolve a code to its i18n label key, falling back to the plain "knock". */
export function knockLabelKey(code: string): string {
  return KNOCKS[code]?.labelKey ?? KNOCKS.knock.labelKey;
}

/** The reply codes for a received knock (empty when it is not a question). */
export function knockReplies(code: string): string[] {
  return KNOCKS[code]?.replies ?? [];
}
