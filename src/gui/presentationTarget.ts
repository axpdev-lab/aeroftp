// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { TID } from '../utils/testIds';
import type { GuiLease } from './controller';
function visualTarget(root: Document, id: string, qualifiers: Record<string, string> = {}): HTMLElement {
    const matches = Array.from(root.querySelectorAll<HTMLElement>('[data-testid]')).filter(element =>
        element.dataset.testid === id && Object.entries(qualifiers).every(([key, value]) => element.getAttribute(key) === value) &&
        !element.closest('[hidden], [aria-hidden="true"], [data-agent="deny"]') && !element.matches(':disabled, [aria-disabled="true"]'));
    if (matches.length !== 1) throw new Error('visual_target_unavailable');
    return matches[0];
}
/** Visual lookup only: never clicks, focuses, scrolls or reads target contents. */
export function presentationPoint(lease: GuiLease, root: Document = document): { x: number; y: number; fallback: boolean } {
    const request = lease.request;
    const args = request?.args ?? {};
    const candidates: (() => HTMLElement)[] = [];
    const panel = typeof args.panel === 'string' ? args.panel : '';
    if (request?.name === 'connect') {
        for (const id of [TID.serverCardConnect, TID.serverRowConnect]) candidates.push(() => visualTarget(root, id, { 'data-profile-id': String(args.profile_id) }));
    } else if (request?.name === 'disconnect') candidates.push(() => visualTarget(root, TID.titlebarDisconnect));
    else if (request?.name === 'select' && Array.isArray(args.names) && args.names.length === 1) {
        candidates.push(() => visualTarget(root, TID.fileRow, { 'data-panel': panel, 'data-file-name': String((args.names as string[])[0]) }));
    } else if (request?.name === 'navigate') {
        candidates.push(() => visualTarget(root, TID.breadcrumbPath, { 'data-panel': panel }));
    } else if (request?.name === 'refresh') candidates.push(() => visualTarget(root, TID.panelRefresh, { 'data-panel': panel }));
    if (panel) candidates.push(() => visualTarget(root, TID.panel, { 'data-panel': panel }));
    for (const find of candidates) {
        try {
            const target = find(); const rect = target.getBoundingClientRect();
            if (target.getClientRects().length && rect.width > 0 && rect.height > 0 && rect.left >= 0 && rect.top >= 0 && rect.right <= innerWidth && rect.bottom <= innerHeight) {
                return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2, fallback: false };
            }
        } catch { /* Missing, denied, ambiguous or offscreen: use the deterministic badge. */ }
    }
    const rect = root.querySelector('[data-gui-controller-badge]')?.getBoundingClientRect();
    return { x: rect ? Math.max(16, rect.left - 20) : 24, y: rect ? Math.max(16, rect.top) : 24, fallback: true };
}
