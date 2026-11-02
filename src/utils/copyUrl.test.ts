// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { remoteCopyUrl } from './copyUrl';
import APP from '../App.tsx?raw';

describe('Copy URL of a remote file', () => {
    it('names a port other than the scheme default, so the CLI does not fall back to it', () => {
        expect(remoteCopyUrl('scp', 'user', 'host', 2222, '/srv/a.txt')).toBe('scp://user@host:2222/srv/a.txt');
        expect(remoteCopyUrl('sftp', 'user', 'host', 2222, '/a')).toBe('sftp://user@host:2222/a');
        expect(remoteCopyUrl('ftp', 'user', 'host', 2121, '/a')).toBe('ftp://user@host:2121/a');
        // The parse the CLI runs reads the same port back.
        expect(new URL(remoteCopyUrl('scp', 'user', 'host', 2222, '/a')).port).toBe('2222');
    });

    it('leaves out the scheme default port and an unset one', () => {
        expect(remoteCopyUrl('scp', 'user', 'host', 22, '/a')).toBe('scp://user@host/a');
        expect(remoteCopyUrl('sftp', 'user', 'host', 22, '/a')).toBe('sftp://user@host/a');
        expect(remoteCopyUrl('ftp', 'user', 'host', 21, '/a')).toBe('ftp://user@host/a');
        expect(remoteCopyUrl('ftp', 'user', 'host', undefined, '/a')).toBe('ftp://user@host/a');
        // SSH's default is not FTP's.
        expect(remoteCopyUrl('ftp', 'user', 'host', 22, '/a')).toBe('ftp://user@host:22/a');
    });

    it('brackets an IPv6 address and keeps a port the server already names', () => {
        expect(remoteCopyUrl('scp', 'user', '2001:db8::1', 2222, '/a')).toBe('scp://user@[2001:db8::1]:2222/a');
        expect(remoteCopyUrl('scp', 'user', '::1', 22, '/a')).toBe('scp://user@[::1]/a');
        expect(remoteCopyUrl('scp', 'user', '[::1]', 2222, '/a')).toBe('scp://user@[::1]:2222/a');
        expect(remoteCopyUrl('ftp', 'user', 'host:2121', 2121, '/a')).toBe('ftp://user@host:2121/a');
        expect(remoteCopyUrl('scp', 'user', '[::1]:2222', 2222, '/a')).toBe('scp://user@[::1]:2222/a');
        expect(new URL(remoteCopyUrl('scp', 'user', '2001:db8::1', 2222, '/a')).hostname).toBe('[2001:db8::1]');
    });

    it('encodes the user and each path segment, so the parse reads the same name back', () => {
        const url = remoteCopyUrl('scp', 'me@corp/x', 'host', 22, "/srv/a dir/what? #1 100%.txt");
        expect(url).toBe('scp://me%40corp%2Fx@host/srv/a%20dir/what%3F%20%231%20100%25.txt');
        const parsed = new URL(url);
        expect(parsed.hostname).toBe('host');
        expect(parsed.search).toBe('');
        expect(parsed.hash).toBe('');
        expect(decodeURIComponent(parsed.username)).toBe('me@corp/x');
        expect(decodeURIComponent(parsed.pathname)).toBe('/srv/a dir/what? #1 100%.txt');
        // Plain names are left as they are.
        expect(remoteCopyUrl('sftp', 'user', 'host', 22, '/srv/a-b_c.txt')).toBe('sftp://user@host/srv/a-b_c.txt');
    });
});

describe('the Copy URL entry of the remote context menu', () => {
    it('builds the URL with remoteCopyUrl and the live connection port', () => {
        expect(APP).toContain('remoteCopyUrl(scheme, currentUsername, currentServer, liveConnectionParams.port, file.path)');
        expect(APP).not.toMatch(/`\$\{scheme\}:\/\//);
    });
});
