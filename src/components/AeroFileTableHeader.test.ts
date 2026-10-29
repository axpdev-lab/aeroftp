// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { TableColumnDef, UseTableColumnsResult } from '../hooks/useTableColumns';

vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));

import { AeroFileTableHeader } from './AeroFileTableHeader';

type ColId = 'name' | 'size' | 'modified';

const COLUMNS: TableColumnDef<ColId>[] = [
    { id: 'name', labelKey: 'browser.name', sortable: true, defaultVisible: true, defaultWidth: 320, minWidth: 140, pinnedStart: true },
    { id: 'size', labelKey: 'browser.size', sortable: true, defaultVisible: true, defaultWidth: 96, minWidth: 70 },
    { id: 'modified', labelKey: 'browser.modified', sortable: true, defaultVisible: true, defaultWidth: 160, minWidth: 110 },
];

const makeColumns = (setWidth: (id: ColId, w: number) => void): UseTableColumnsResult<ColId> => ({
    config: {
        visibility: { name: true, size: true, modified: true },
        order: ['name', 'size', 'modified'],
        widths: { name: 320, size: 96, modified: 160 },
        sort: null,
        aligns: {},
    },
    orderedVisibleColumns: COLUMNS,
    orderedAllColumns: COLUMNS,
    setVisible: vi.fn(),
    setOrder: vi.fn(),
    setWidth,
    setSort: vi.fn(),
    setAlign: vi.fn(),
    resolveAlign: () => 'left',
    reset: vi.fn(),
    hasPersisted: false,
});

let root: Root;
let host: HTMLDivElement;
let setWidth: ReturnType<typeof vi.fn<(id: ColId, w: number) => void>>;
let onLiveResize: ReturnType<typeof vi.fn<(id: ColId, w: number) => void>>;

beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.appendChild(host);
    root = createRoot(host);
    setWidth = vi.fn<(id: ColId, w: number) => void>();
    onLiveResize = vi.fn<(id: ColId, w: number) => void>();
    const table = createElement('table', null, createElement(AeroFileTableHeader<ColId>, {
        columns: makeColumns(setWidth),
        onLiveResize,
    }));
    act(() => root.render(table));
});

afterEach(() => {
    act(() => root.unmount());
    host.remove();
});

const headerCells = () => Array.from(host.querySelectorAll('th'));
const gripsIn = (el: Element) => Array.from(el.querySelectorAll('[role="separator"]'));

describe('AeroFileTableHeader column grips', () => {
    it('places the last column grip next to the gear in the sticky tools block (classes and DOM placement only, not rendered layout)', () => {
        const cells = headerCells();
        const last = cells[cells.length - 1];
        const tools = last.querySelector('[data-testid="aerofile-header-last-column-tools"]');
        expect(tools).not.toBeNull();
        expect(tools!.className).toContain('sticky');
        expect(tools!.className).toContain('right-0');
        expect(tools!.className).toContain('-my-2');
        expect(tools!.className).toContain('py-2');
        expect(tools!.querySelector('button[title="table.manageColumns"]')).not.toBeNull();
        expect(gripsIn(tools!)).toHaveLength(1);
        expect(gripsIn(last)).toHaveLength(1);
    });

    it('leaves every other grip on its own header cell edge', () => {
        const cells = headerCells();
        for (const th of cells.slice(0, -1)) {
            const grips = gripsIn(th);
            expect(grips).toHaveLength(1);
            expect(grips[0].parentElement).toBe(th);
        }
    });

    it('marks the header row so hovering it reveals every divider', () => {
        const row = host.querySelector('thead tr');
        expect(row!.className).toContain('group/colheader');
        const grips = gripsIn(host);
        expect(grips).toHaveLength(COLUMNS.length);
        for (const grip of grips) {
            expect(grip.querySelector('span')!.className).toContain('group-hover/colheader:bg-gray-300');
        }
    });

    it('shrinks the last column from its grip and clamps to the minimum width', () => {
        const cells = headerCells();
        const grip = gripsIn(cells[cells.length - 1])[0];
        act(() => {
            grip.dispatchEvent(new MouseEvent('pointerdown', { bubbles: true, clientX: 500 }));
        });
        act(() => {
            window.dispatchEvent(new MouseEvent('pointermove', { clientX: 470 }));
        });
        expect(onLiveResize).toHaveBeenLastCalledWith('modified', 130);
        act(() => {
            window.dispatchEvent(new MouseEvent('pointermove', { clientX: 300 }));
            window.dispatchEvent(new MouseEvent('pointerup', {}));
        });
        expect(onLiveResize).toHaveBeenLastCalledWith('modified', 110);
        expect(setWidth).toHaveBeenCalledWith('modified', 110);
    });
});
