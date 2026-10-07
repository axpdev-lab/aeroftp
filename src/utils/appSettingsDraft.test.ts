// @vitest-environment jsdom
import { expect, it, vi } from 'vitest';
import { mergeAppSettingsDraft } from './appSettingsDraft';
import { updateAppSettings, commitAppSettings } from './appSettings';
import { ConnectScope } from '../gui/connectScope';
const backend = vi.hoisted(() => ({ blob: { fontSize: 14, dateFormat: 'localized', confirmBeforeDelete: true }, writes: 0 }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async (command: string, args?: { password: string }) => {
    if (command === 'get_credential') return JSON.stringify(backend.blob);
    if (command === 'store_credential') { ++backend.writes; backend.blob = JSON.parse(args!.password); }
}) }));
it('rebases a human draft and preserves concurrent unrelated agent settings and policy', async () => {
    const base = { ...backend.blob };
    const edited = { ...base, fontSize: 18 };
    await commitAppSettings(async () => backend.blob = { ...backend.blob, dateFormat: 'iso' }, new ConnectScope());
    expect(mergeAppSettingsDraft(base, edited, backend.blob)).toEqual({ fontSize: 18, dateFormat: 'iso', confirmBeforeDelete: true });
    await updateAppSettings(existing => mergeAppSettingsDraft(base, edited, existing as typeof base), undefined, true);
    expect(backend.blob).toEqual({ fontSize: 18, dateFormat: 'iso', confirmBeforeDelete: true });
});
it('an external publication retains dirty fields but adopts unedited fields', () => {
    const base = { fontSize: 14, compactMode: false, confirmBeforeDelete: true };
    expect(mergeAppSettingsDraft(base, { ...base, compactMode: true }, { ...base, fontSize: 16 }))
        .toEqual({ fontSize: 16, compactMode: true, confirmBeforeDelete: true });
});
it('keeps typing after a captured save pending against the acknowledged baseline', () => {
    const captured = { fontSize: 18, dateFormat: 'localized' };
    expect(mergeAppSettingsDraft(captured, { ...captured, fontSize: 20 }, { ...captured, dateFormat: 'iso' }))
        .toEqual({ fontSize: 20, dateFormat: 'iso' });
});

it('a queued agent commit merges against the acknowledged earlier human save', async () => {
    backend.blob = { fontSize: 14, dateFormat: 'localized', confirmBeforeDelete: true };
    let release!: () => void; let entered!: () => void;
    const started = new Promise<void>(resolve => { entered = resolve; });
    const blocked = new Promise<void>(resolve => { release = resolve; });
    const human = commitAppSettings(async () => {
        entered(); await blocked;
        return backend.blob = { ...backend.blob, dateFormat: 'iso' };
    }, new ConnectScope());
    await started;
    const agent = commitAppSettings(async () => backend.blob = { ...backend.blob, fontSize: 20 }, new ConnectScope());
    expect(backend.blob.fontSize).toBe(14);
    release(); await human; await agent;
    expect(backend.blob).toEqual({ fontSize: 20, dateFormat: 'iso', confirmBeforeDelete: true });
});
