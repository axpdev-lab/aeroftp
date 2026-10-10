// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { afterEach, expect, it, vi } from 'vitest';
import { presentationPoint } from './presentationTarget';
import { TID } from '../utils/testIds';
import { LOCAL_GUI_ACTOR, type GuiLease } from './controller';
const lease: GuiLease = { owner: LOCAL_GUI_ACTOR, intent: 'connect', request: { name: 'connect', args: { profile_id: 'srv_1' } } };
function target(profile: string) {
    const el = document.createElement('button'); el.dataset.testid = TID.serverCardConnect; el.dataset.profileId = profile;
    document.body.append(el);
    el.getBoundingClientRect = () => ({ left: 50, top: 50, right: 150, bottom: 90, width: 100, height: 40 } as DOMRect);
    el.getClientRects = () => [el.getBoundingClientRect()] as unknown as DOMRectList;
    return el;
}
afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });
it('points at the exact profile without clicks, focus or scrolling', () => {
    target('srv_2'); const exact = target('srv_1');
    const click = vi.spyOn(exact, 'click'), focus = vi.spyOn(exact, 'focus');
    expect(presentationPoint(lease)).toEqual({ x: 100, y: 70, fallback: false });
    expect(click).not.toHaveBeenCalled(); expect(focus).not.toHaveBeenCalled();
});
it('uses the badge for missing, ambiguous, denied and offscreen addresses', () => {
    const badge = document.createElement('div'); badge.dataset.guiControllerBadge = '';
    badge.getBoundingClientRect = () => ({ left: 200, top: 300 } as DOMRect); document.body.append(badge);
    const fallback = { x: 180, y: 300, fallback: true };
    expect(presentationPoint(lease)).toEqual(fallback);
    const a = target('srv_1'), b = target('srv_1'); expect(presentationPoint(lease)).toEqual(fallback);
    b.remove(); a.dataset.agent = 'deny'; expect(presentationPoint(lease)).toEqual(fallback);
    delete a.dataset.agent;
    a.getBoundingClientRect = () => ({ left: -10, top: 0, right: 90, bottom: 40, width: 100, height: 40 } as DOMRect);
    expect(presentationPoint(lease)).toEqual(fallback);
});
