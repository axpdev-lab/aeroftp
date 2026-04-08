// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)
//
// Invisible resize handles for undecorated windows (decorations: false).
// Uses pointer capture + movementX/movementY deltas because WebKitGTK on
// Wayland doesn't report correct screenX/screenY absolute coordinates.

import * as React from 'react';
import { getCurrentWindow, LogicalSize, LogicalPosition } from '@tauri-apps/api/window';

const EDGE = 6;
const CORNER = 12;

type Dir = 'n' | 's' | 'e' | 'w' | 'ne' | 'nw' | 'se' | 'sw';

const cursors: Record<Dir, string> = {
  n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize',
  ne: 'nesw-resize', nw: 'nwse-resize', se: 'nwse-resize', sw: 'nesw-resize',
};

// Only needed on Linux — Windows (DWM) and macOS (AppKit) provide native
// resize zones even with decorations: false.
const isLinux = navigator.userAgent.includes('Linux');

interface EdgeProps {
  dir: Dir;
  style: React.CSSProperties;
}

const ResizeEdge: React.FC<EdgeProps> = ({ dir, style }) => {
  const ref = React.useRef<HTMLDivElement>(null);

  const onPointerDown = React.useCallback((e: React.PointerEvent) => {
    e.preventDefault();
    e.stopPropagation();

    const el = ref.current;
    if (!el) return;

    el.setPointerCapture(e.pointerId);

    const win = getCurrentWindow();
    const hasN = dir.includes('n');
    const hasS = dir.includes('s');
    const hasW = dir === 'w' || dir === 'nw' || dir === 'sw';
    const hasE = dir === 'e' || dir === 'ne' || dir === 'se';

    let currentW = 0, currentH = 0;
    let currentX = 0, currentY = 0;
    let scale = 1;
    let raf = 0;
    let accumDx = 0, accumDy = 0;

    Promise.all([win.innerSize(), win.outerPosition(), win.scaleFactor()]).then(
      ([size, pos, sf]) => {
        scale = sf;
        currentW = size.width / scale;
        currentH = size.height / scale;
        currentX = pos.x / scale;
        currentY = pos.y / scale;

        const applyResize = () => {
          raf = 0;
          const dx = accumDx;
          const dy = accumDy;
          accumDx = 0;
          accumDy = 0;

          if (hasE) currentW = Math.max(400, currentW + dx);
          if (hasW) {
            const newW = Math.max(400, currentW - dx);
            currentX += currentW - newW;
            currentW = newW;
          }
          if (hasS) currentH = Math.max(600, currentH + dy);
          if (hasN) {
            const newH = Math.max(600, currentH - dy);
            currentY += currentH - newH;
            currentH = newH;
          }

          win.setSize(new LogicalSize(Math.round(currentW), Math.round(currentH)));
          if (hasN || hasW) {
            win.setPosition(new LogicalPosition(Math.round(currentX), Math.round(currentY)));
          }
        };

        const onMove = (ev: PointerEvent) => {
          accumDx += ev.movementX;
          accumDy += ev.movementY;
          if (!raf) raf = requestAnimationFrame(applyResize);
        };

        const onUp = () => {
          el.removeEventListener('pointermove', onMove);
          el.removeEventListener('pointerup', onUp);
          el.removeEventListener('lostpointercapture', onUp);
          if (raf) cancelAnimationFrame(raf);
          document.body.style.cursor = '';
          document.body.style.userSelect = '';
        };

        document.body.style.cursor = cursors[dir];
        document.body.style.userSelect = 'none';
        el.addEventListener('pointermove', onMove);
        el.addEventListener('pointerup', onUp);
        el.addEventListener('lostpointercapture', onUp);
      }
    );
  }, [dir]);

  return (
    <div
      ref={ref}
      style={{ position: 'fixed', zIndex: 9999, cursor: cursors[dir], touchAction: 'none', ...style }}
      onPointerDown={onPointerDown}
    />
  );
};

export const WindowResizeEdges: React.FC = React.memo(() => {
  if (!isLinux) return null;
  return (
    <>
      {/* Edges */}
      <ResizeEdge dir="n"  style={{ top: 0, left: CORNER, right: CORNER, height: EDGE }} />
      <ResizeEdge dir="s"  style={{ bottom: 0, left: CORNER, right: CORNER, height: EDGE }} />
      <ResizeEdge dir="w"  style={{ left: 0, top: CORNER, bottom: CORNER, width: EDGE }} />
      <ResizeEdge dir="e"  style={{ right: 0, top: CORNER, bottom: CORNER, width: EDGE }} />
      {/* Corners */}
      <ResizeEdge dir="nw" style={{ top: 0, left: 0, width: CORNER, height: CORNER }} />
      <ResizeEdge dir="ne" style={{ top: 0, right: 0, width: CORNER, height: CORNER }} />
      <ResizeEdge dir="sw" style={{ bottom: 0, left: 0, width: CORNER, height: CORNER }} />
      <ResizeEdge dir="se" style={{ bottom: 0, right: 0, width: CORNER, height: CORNER }} />
    </>
  );
});
