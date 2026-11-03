// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import snapcraftYaml from '../../snap/snapcraft.yaml?raw';
import tauriConf from '../../src-tauri/tauri.conf.json?raw';
import cargoToml from '../../src-tauri/Cargo.toml?raw';
import libRs from '../../src-tauri/src/lib.rs?raw';

// tauri-plugin-single-instance asks the session bus for `<identifier>.SingleInstance`
// (plus a version suffix with its `semver` feature, or another base when given a
// dbus id). In the snap AppArmor grants that name only through the `dbus` slot, so
// the slot must name exactly what the plugin asks for, or every launch is a new
// instance again.
describe('snap single-instance slot', () => {
    it('names the bus name the single-instance plugin asks for', () => {
        const identifier: string = JSON.parse(tauriConf).identifier;
        const slots = snapcraftYaml.slice(snapcraftYaml.indexOf('\nslots:'), snapcraftYaml.indexOf('\nlayout:'));
        expect(slots).toMatch(/^ {2}single-instance:\n {4}interface: dbus\n {4}bus: session\n/m);
        expect(slots).toMatch(new RegExp(`^ {4}name: ${identifier.replace(/\./g, '\\.')}\\.SingleInstance$`, 'm'));
    });

    it('keeps the plugin on the default name', () => {
        expect(cargoToml).toMatch(/^tauri-plugin-single-instance = "[^"]+"$/m);
        expect(libRs).toContain('.plugin(tauri_plugin_single_instance::init(');
        expect(libRs).not.toMatch(/tauri_plugin_single_instance::Builder/);
    });
});
