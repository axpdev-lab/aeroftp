// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { JSDOM } from 'jsdom';
const read = (path: string) => readFileSync(path, 'utf8');
const catalog = JSON.parse(read('src-tauri/installer/association-registry.json')) as {
  owned: { extension: string; progid: string; aliases: string[] }[];
  archives: string[];
  values: { group: string; key: string; name: string; value: string }[];
};
describe('Windows installer / Settings registration contract', () => {
  it('requires upstream template review when Tauri CLI changes', () => {
    const pin = JSON.parse(read('src-tauri/installer/TEMPLATE-UPSTREAM.json'));
    const lock = JSON.parse(read('package-lock.json'));
    expect(pin.tauriCliVersion).toBe(lock.packages['node_modules/@tauri-apps/cli'].version);
  });
  it('ships generated NSIS and MSI registrations from the same catalog as the backend', () => {
    expect(() => execFileSync(process.execPath, ['scripts/generate-windows-associations.mjs', '--check'], { stdio: 'pipe' })).not.toThrow();
    expect(read('src-tauri/src/file_associations_windows.rs')).toContain('include_str!("../installer/association-registry.json")');
  });
  it('covers every catalog value exactly once and shares common values across either MSI choice', () => {
    const xml = new JSDOM(read('src-tauri/installer/associations.wxs'), { contentType: 'text/xml' }).window.document;
    const components = Array.from(xml.querySelectorAll('Component'));
    const values = components.flatMap(component => Array.from(component.querySelectorAll('RegistryValue')));
    for (const value of catalog.values) {
      const matches = values.filter(node => node.getAttribute('Key') === value.key && (node.getAttribute('Name') || '') === value.name);
      expect(matches).toHaveLength(1);
    }
    for (const [owned, archives] of [[false, false], [true, false], [false, true], [true, true]]) {
      const enabled = new Set([
        ...(owned ? ['AeroFTPOwnedAssociations'] : []),
        ...(archives ? ['AeroFTPArchiveAssociations'] : []),
      ]);
      const refs = new Set(Array.from(xml.querySelectorAll('Feature')).filter(feature => enabled.has(feature.getAttribute('Id')!))
        .flatMap(feature => Array.from(feature.querySelectorAll('ComponentRef')).map(ref => ref.getAttribute('Id'))));
      const active = components.filter(component => refs.has(component.getAttribute('Id'))).flatMap(component => Array.from(component.querySelectorAll('RegistryValue')));
      for (const value of catalog.values) {
        const expected = value.group === 'owned' ? owned : value.group === 'archives' ? archives : owned || archives;
        expect(active.some(node => node.getAttribute('Key') === value.key && (node.getAttribute('Name') || '') === value.name)).toBe(expected);
      }
    }
  });
  it('leaves MSI opt-out features published and selectable for later maintenance', () => {
    const xml = new JSDOM(read('src-tauri/installer/associations.wxs'), { contentType: 'text/xml' }).window.document;
    for (const feature of xml.querySelectorAll('Feature')) {
      expect(feature.getAttribute('Absent')).toBe('allow');
      expect(feature.getAttribute('Level')).toBe('1');
      const condition = feature.querySelector('Condition')!;
      expect(condition.getAttribute('Level')).toBe('2');
      expect(condition.textContent).toBe('AEROFTP_NOASSOC = "1"');
    }
  });
  it('covers every bundled AeroFTP type and never sets an extension default', () => {
    const config = JSON.parse(read('src-tauri/tauri.conf.json'));
    expect(catalog.owned.map(t => t.extension).sort()).toEqual(config.bundle.fileAssociations.flatMap((item: { ext: string[] }) => item.ext).sort());
    const identities = new Set<string>();
    for (const item of catalog.values) {
      expect(['owned', 'archives', 'common']).toContain(item.group);
      expect(item.key).not.toMatch(/UserChoice/);
      expect(item.key.startsWith('Software\\')).toBe(true);
      expect(item.name !== '' || !/^Software\\Classes\\\.[^\\]+$/.test(item.key)).toBe(true);
      const identity = `${item.key.toLowerCase()}\0${item.name.toLowerCase()}`;
      expect(identities.has(identity)).toBe(false); identities.add(identity);
    }
  });
  it('retains every old NSIS and MSI ProgID so existing default choices keep opening', () => {
    for (const item of catalog.owned) {
      const ids = [item.progid, ...item.aliases].map(id => id.toLowerCase());
      expect(ids).toContain(item.extension.toLowerCase());
      expect(ids).toContain(`aeroftp.${item.extension}`.toLowerCase());
      for (const id of ids) {
        expect(catalog.values.some(value => value.group === 'owned' && value.key.toLowerCase() === `software\\classes\\${id}\\shell\\open\\command` && value.value === '"@EXE@" "%1"')).toBe(true);
      }
    }
  });
  it('keeps the application and CLI binaries in the mandatory MSI feature', () => {
    const mandatory = read('src-tauri/installer/main.wxs').split('Id="MainProgram"')[1].split('<Feature Id="ShortcutsFeature"')[0];
    expect(mandatory).toContain('<ComponentRef Id="Path"/>');
    expect(mandatory).toContain('<ComponentRef Id="{{ bin.id }}"/>');
    expect(read('src-tauri/installer/main.wxs')).toContain('Property="ARPINSTALLLOCATION" Value="[INSTALLDIR]"');
    expect(read('src-tauri/src/file_associations_windows.rs')).toContain('cmd.raw_arg(directory_property)');
    expect(read('src-tauri/installer/main.wxs')).toContain('NOT INSTALLDIR AND AEROFTP_PREVINSTALLDIR');
  });
  it('gives both installers real choices rather than retaining automatic default registrations', () => {
    const config = JSON.parse(read('src-tauri/tauri.conf.json'));
    expect(config.bundle.windows.nsis.template).toBe('installer/installer.nsi');
    expect(config.bundle.windows.wix.template).toBe('installer/main.wxs');
    expect(config.bundle.windows.wix.featureRefs).toEqual(['AeroFTPOwnedAssociations', 'AeroFTPArchiveAssociations']);
    expect(read('src-tauri/installer/installer.nsi')).not.toContain('!insertmacro APP_ASSOCIATE ');
    expect(read('src-tauri/installer/installer.nsi')).not.toContain('!insertmacro APP_UNASSOCIATE ');
    expect(read('src-tauri/installer/main.wxs')).not.toContain('{{#each file_associations');
    expect(read('src-tauri/installer/main.wxs')).toContain('WixUI_FeatureTree');
  });
});
