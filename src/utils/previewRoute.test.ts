// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Pins the destination of a "show me this file" gesture (#347, Ehud,
// 2026-08-06). The defect these guard against: a .txt opened in Universal
// Preview while a .sh was handed to the AeroTools editor, so the same
// double-click produced two unrelated outcomes depending on the extension, and
// with the Editor toggled off the .sh case showed nothing at all.
//
// Written against the shared rule rather than any one panel, because the rule
// had been copied into four call sites and drifted there.

import { describe, it, expect } from 'vitest';
import { previewRouteFor } from './previewRoute';
import { isPreviewable as isEditorPreviewable } from '../components/DevTools/types';

describe('previewRouteFor', () => {
    it('sends a shell script to the preview, the same place a .txt goes', () => {
        // The reported case. Before the fix this was 'editor'.
        expect(previewRouteFor('deploy.sh')).toBe('universal-preview');
        expect(previewRouteFor('notes.txt')).toBe('universal-preview');
    });

    it('sends every text-based kind to the same surface', () => {
        for (const name of [
            'run.sh', 'run.bash', 'run.zsh', 'build.ps1', 'setup.py', 'main.rs',
            'index.ts', 'app.tsx', 'style.css', 'page.html', 'data.json',
            'config.yaml', 'Cargo.toml', 'query.sql', 'README.md', 'notes.txt',
            'server.log', 'app.ini', 'rows.csv',
        ]) {
            expect(previewRouteFor(name), name).toBe('universal-preview');
        }
    });

    it('keeps media, PDF and images on the preview route', () => {
        // Guards the mistake of narrowing the preview arm to text: these had
        // always previewed and must keep doing so.
        for (const name of ['clip.mp4', 'song.mp3', 'photo.jpg', 'scan.pdf', 'icon.svg']) {
            expect(previewRouteFor(name), name).toBe('universal-preview');
        }
    });

    it('previews the text formats only the editor used to recognise', () => {
        // These live in the editor's Monaco language map but were absent from
        // the preview's own extension table, so before the fix each opened in
        // the editor while the .txt beside it opened in the preview.
        for (const name of [
            'main.tf', 'vars.tfvars', 'stack.bicep', 'schema.proto',
            'token.sol', 'unit.pas', 'core.scm', 'queue.redis', 'view.cshtml',
        ]) {
            expect(previewRouteFor(name), name).toBe('universal-preview');
        }
    });

    it('renders everything the editor can open', () => {
        // The containment that lets the route drop its editor arm: if the
        // editor recognises a name, the preview must render it, or a
        // double-click on that file would open nothing at all. This corpus
        // covers each source the editor draws on (language map, its own extra
        // extension list, and its extension-less filenames).
        for (const name of [
            'deploy.sh', 'main.tf', 'token.sol', 'notes.txt', 'server.log',
            'settings.example', 'config.sample', 'old.bak',
            'Makefile', 'Dockerfile', 'Containerfile', 'Vagrantfile', 'Gemfile',
            'Rakefile', 'Procfile', 'Brewfile', 'Justfile',
            'LICENSE', 'LICENCE', 'AUTHORS', 'CONTRIBUTORS',
            'CHANGELOG', 'CHANGES', 'README', 'TODO',
            'icon.png', 'shot.jpg', 'anim.gif', 'art.webp', 'fav.ico', 'bit.bmp',
        ]) {
            if (isEditorPreviewable(name)) {
                expect(previewRouteFor(name), name).toBe('universal-preview');
            }
        }
    });

    it('does nothing for a file no surface can render', () => {
        for (const name of ['archive.zip', 'installer.exe', 'blob.bin']) {
            expect(previewRouteFor(name), name).toBe('none');
        }
    });

    it('does not care about the path in front of the name', () => {
        expect(previewRouteFor('/home/user/scripts/deploy.sh')).toBe('universal-preview');
        expect(previewRouteFor('C:\\scripts\\deploy.sh')).toBe('universal-preview');
    });
});
