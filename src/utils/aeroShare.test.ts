// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
  extractTicketFromLink,
  looksLikeShareLink,
  shortAfid,
  peerProfileId,
  isFriendProfile,
  friendCanConnect,
  SHARE_LINK_PREFIX,
} from './aeroShare';

// A realistic ticket + capability, mirroring the round-trip test in
// src-tauri/src/peer_commands.rs (share_link_round_trips). The frontend treats
// the link as opaque but must recover the ticket for provider_connect, so the
// split has to match the LOCKED `aeroftp-share://v1/<ticket>/<cap>` format.
const TICKET = 'docaaa7uzbzx6lcccbtaqki6uynkb6cocjabkzkmpkzcz3ifyu5fgza';
const CAP = 'q83vEjRWshESNFZ4kBI0VniQ';
const LINK = `${SHARE_LINK_PREFIX}${TICKET}/${CAP}`;

describe('aeroShare share-link helpers', () => {
  it('recognises a share link, tolerant of surrounding whitespace', () => {
    expect(looksLikeShareLink(LINK)).toBe(true);
    expect(looksLikeShareLink(`  ${LINK}\n`)).toBe(true);
    expect(looksLikeShareLink('https://example.com/x')).toBe(false);
    expect(looksLikeShareLink('')).toBe(false);
  });

  it('extracts the ticket from a well-formed link', () => {
    expect(extractTicketFromLink(LINK)).toBe(TICKET);
    expect(extractTicketFromLink(`\t${LINK}  `)).toBe(TICKET);
  });

  it('fails closed on malformed links', () => {
    expect(extractTicketFromLink('https://example.com/x')).toBeNull();
    expect(extractTicketFromLink(`${SHARE_LINK_PREFIX}onlyticket`)).toBeNull();
    expect(extractTicketFromLink(`${SHARE_LINK_PREFIX}/cap`)).toBeNull();
    expect(extractTicketFromLink(`${SHARE_LINK_PREFIX}${TICKET}/`)).toBeNull();
    expect(extractTicketFromLink('')).toBeNull();
  });
});

describe('aeroShare AFID helpers', () => {
  it('shortens long AeroFTP-IDs to ABCD1234...WXYZ form', () => {
    const long = 'AFID1Y6pmnpUPJNqmDhjN7pEuGc4xyCmzWnSWF6T9Gg6heCDUE6j86K804ZR64BEz0N';
    const s = shortAfid(long);
    expect(s.startsWith('AFID1Y6p')).toBe(true);
    expect(s.endsWith('Ez0N')).toBe(true);
    expect(s).toContain('…');
    // Short IDs are returned verbatim (no ellipsis).
    expect(shortAfid('AFID1abc')).toBe('AFID1abc');
  });

  it('keys the friend profile id by AFID for idempotent upserts', () => {
    expect(peerProfileId('AFID1abc')).toBe('peer_AFID1abc');
  });
});

describe('aeroShare profile predicates', () => {
  it('detects friend profiles by protocol', () => {
    expect(isFriendProfile({ protocol: 'peer' })).toBe(true);
    expect(isFriendProfile({ protocol: 'ftp' })).toBe(false);
    expect(isFriendProfile({})).toBe(false);
  });

  it('treats a friend as connectable only with a complete receive binding', () => {
    // All three parts present -> connectable (click connects to the replica).
    expect(friendCanConnect({ options: { peerNamespace: 'ns', peerTicket: 't', peerLocalFolder: '/x' } })).toBe(true);
    // Missing any part -> not connectable (click opens the handshake dialog).
    expect(friendCanConnect({ options: { peerNamespace: 'ns', peerTicket: 't' } })).toBe(false);
    expect(friendCanConnect({ options: { peerNamespace: 'ns', peerLocalFolder: '/x' } })).toBe(false);
    expect(friendCanConnect({ options: {} })).toBe(false);
    expect(friendCanConnect({})).toBe(false);
  });
});
