// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';

/**
 * No component or hook writes a ref while it renders.
 *
 * React can render and then discard the result, and a ref written during that
 * render keeps a value from a tree that never committed: a callback, the
 * options or the rows of a render that was thrown away. A latest-value ref is
 * written in a layout effect, once the render commits and before an event can
 * read it. Lazy initialisation is allowed, as React allows it: a write in the
 * branch where the same ref was found empty (`if (ref.current === null)
 * ref.current = ...`, `if (!ref.current) ...`), or `ref.current ??= ...`.
 *
 * The test parses every source file and lists each write to `x.current`
 * (`=`, `+=`, `++`, `x['current']`) made by a component or a hook (`Name`,
 * `useName`) while it renders: in its own body, or in a function it runs
 * while rendering (a `useMemo` factory, a state initialiser, an array
 * callback, a function called on the spot). A callback or an effect inside it
 * runs later and is left alone.
 */

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.ts', '/src/**/*.tsx', '!/src/**/*.test.ts', '!/src/**/*.test.tsx', '!/src/**/*.d.ts'],
    { query: '?raw', import: 'default', eager: true },
);

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';
const FUNCTION = /^(FunctionDeclaration|FunctionExpression|ArrowFunctionExpression)$/;

/** The name a function is declared or bound under, through one call (`memo(() => ...)`). */
function functionName(fn: Node, ancestors: Node[]): string | null {
    const id = fn.id as Node | null | undefined;
    if (id && id.type === 'Identifier') return id.name as string;
    let parent = ancestors[ancestors.length - 1];
    if (parent?.type === 'CallExpression') parent = ancestors[ancestors.length - 2];
    const bound = parent?.type === 'VariableDeclarator' ? parent.id as Node : null;
    return bound?.type === 'Identifier' ? bound.name as string : null;
}

/** `useMemo` for `useMemo(...)` and `React.useMemo(...)`, `map` for `rows.map(...)`. */
function calleeName(call: Node): string | null {
    const callee = call.callee as Node;
    if (callee.type === 'Identifier') return callee.name as string;
    if (callee.type === 'MemberExpression' && !callee.computed) return (callee.property as Node).name as string;
    return null;
}

/** Calls that run the function they are given before they return. */
const RUNS_ON_THE_SPOT = new Set([
    'useMemo', 'useState', 'useReducer', 'useSyncExternalStore',
    'map', 'flatMap', 'forEach', 'filter', 'reduce', 'reduceRight', 'some', 'every',
    'find', 'findIndex', 'findLast', 'findLastIndex', 'sort', 'toSorted',
]);

/** A function its caller runs before going on: called where it is written, or handed to one of those calls. */
function runsOnTheSpot(fn: Node, parent: Node | undefined): boolean {
    if (parent?.type !== 'CallExpression') return false;
    if (parent.callee === fn) return true;
    const name = calleeName(parent);
    return !!name && RUNS_ON_THE_SPOT.has(name) && (parent.arguments as Node[]).includes(fn);
}

/** The ref of `x.current` or `x['current']`, as written (`x`), or null for any other target. */
function refOf(source: string, target: Node | undefined): string | null {
    if (target?.type !== 'MemberExpression') return null;
    const property = target.property as Node;
    const key = target.computed ? (property.type === 'Literal' ? property.value : null) : property.name;
    if (key !== 'current') return null;
    const object = target.object as Node;
    return source.slice(object.start, object.end);
}

/** `ref.current === null`, `ref.current == undefined`, `null === ref.current`, `!ref.current`. */
function findsEmpty(source: string, test: Node, ref: string): boolean {
    if (test.type === 'UnaryExpression' && test.operator === '!') return refOf(source, test.argument as Node) === ref;
    if (test.type !== 'BinaryExpression' || (test.operator !== '===' && test.operator !== '==')) return false;
    const empty = (side: Node) => (side.type === 'Literal' && side.value === null && source.slice(side.start, side.end) === 'null')
        || (side.type === 'Identifier' && side.name === 'undefined');
    const [left, right] = [test.left as Node, test.right as Node];
    return (refOf(source, left) === ref && empty(right)) || (refOf(source, right) === ref && empty(left));
}

type RefWrite = {
    line: number;
    ref: string;
    text: string;
    /** The component or hook whose render makes the write, or the name of the function that makes it later. */
    owner: string | null;
    /** Lazy initialisation: the write fills the ref only when it is empty. */
    lazy: boolean;
    /** Written directly in a callback handed to `useLayoutEffect`. */
    inLayoutEffect: boolean;
};

function refWrites(file: string, source: string): RefWrite[] {
    const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
    const writes: RefWrite[] = [];
    const visit = (node: Node, ancestors: Node[]) => {
        const target = (node.type === 'AssignmentExpression' ? node.left : node.type === 'UpdateExpression' ? node.argument : undefined) as Node | undefined;
        const ref = refOf(source, target);
        if (ref !== null) {
            let nearest = ancestors.length - 1;
            while (nearest >= 0 && !FUNCTION.test(ancestors[nearest].type)) nearest--;
            let owner = nearest;
            while (owner >= 0 && runsOnTheSpot(ancestors[owner], ancestors[owner - 1])) {
                owner--;
                while (owner >= 0 && !FUNCTION.test(ancestors[owner].type)) owner--;
            }
            const path = [...ancestors, node];
            const lazy = node.operator === '??=' || node.operator === '||=' || path.slice(Math.max(owner, 0)).some((a, k, rest) => {
                const consequent = a.consequent as Node | undefined;
                return a.type === 'IfStatement' && rest[k + 1] === consequent && findsEmpty(source, a.test as Node, ref);
            });
            const parent = ancestors[nearest - 1];
            writes.push({
                line: source.slice(0, node.start).split('\n').length,
                ref,
                text: source.slice(node.start, node.end).split('\n')[0],
                owner: owner >= 0 ? functionName(ancestors[owner], ancestors.slice(0, owner)) : null,
                lazy,
                inLayoutEffect: nearest >= 0 && parent?.type === 'CallExpression' && calleeName(parent) === 'useLayoutEffect',
            });
        }
        for (const value of Object.values(node)) {
            if (Array.isArray(value)) value.filter(isNode).forEach(child => visit(child, [...ancestors, node]));
            else if (isNode(value)) visit(value, [...ancestors, node]);
        }
    };
    visit(program, []);
    return writes;
}

function renderTimeRefWrites(file: string, source: string): string[] {
    return refWrites(file, source)
        .filter(w => w.owner && /^([A-Z]|use[A-Z])/.test(w.owner) && !w.lazy)
        .map(w => `${file}:${w.line} ${w.owner}: ${w.text}`);
}

/**
 * Latest-value refs: each follows a value of the render, and a callback, a
 * listener or a timer reads it later. App's, and the ones moved out of the
 * render into a layout effect. Without that effect a ref keeps the value of
 * the first render, and no write during render is left to report.
 */
const LATEST_VALUE_REFS: Record<string, string[]> = {
    '/src/App.tsx': ['sortedLocalFilesRef', 'sortedRemoteFilesRef', 'handleToggleAeroFileRef', 'transferFnsRef', 'crossPanelDropRef', 'lockControllerInterrupt'],
    '/src/components/AISettings/AISettingsPanel.tsx': ['settingsRef'],
    '/src/components/AeroSync/DeltaEligibilityDialog.tsx': ['skipFutureRef'],
    '/src/components/ConnectionScreen.tsx': ['onTabLabelChangeRef'],
    '/src/components/DevTools/AIChat.tsx': ['toolRegistryRef', 'rowHandlersRef'],
    '/src/components/DevTools/ChatSearchOverlay.tsx': ['onSearchResultsRef', 'onHighlightMessageRef'],
    '/src/components/DevTools/useAIChatConversations.ts': ['messagesRef', 'conversationsRef', 'activeConversationIdRef'],
    '/src/components/GitHubActionsBrowser.tsx': ['runsRef'],
    '/src/components/PlacesSidebar.tsx': ['activePortableDeviceIdRef', 'onPortableDeviceClosedRef'],
    '/src/components/Preview/controls/WebGLVisualizer.tsx': ['tRef'],
    '/src/components/Preview/viewers/CropOverlay.tsx': ['cropRef'],
    '/src/components/Preview/viewers/ImageViewer.tsx': ['baseSrcRef', 'zoomRef', 'positionRef', 'navRef', 'wheelState'],
    '/src/components/SettingsPanel.tsx': ['settingsDraft', 'panelOpen'],
    '/src/components/ShareLinkModal.tsx': ['humanLogRef'],
    '/src/components/Toast.tsx': ['onCloseRef'],
    '/src/hooks/useAutoUpdate.ts': ['activityLogRef'],
    '/src/hooks/useCloudSync.ts': ['callbacksRef'],
    '/src/hooks/useFaviconDetection.ts': ['sessionsRef', 'callbackRef'],
    '/src/hooks/useFontSizeShortcuts.tsx': ['fontSizeRef'],
    '/src/hooks/useLockNow.ts': ['current'],
    '/src/hooks/useMarqueeSelection.ts': ['selectedRef', 'moveHandlerRef', 'upHandlerRef'],
    '/src/hooks/usePointerDrag.ts': ['handlersRef'],
    '/src/hooks/useTransferEvents.ts': ['optRef'],
};

describe('no ref is written during render', () => {
    it('reads the sources', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(Object.keys(SOURCES)).toContain('/src/App.tsx');
    });

    it('writes no ref during render', () => {
        const writes = Object.entries(SOURCES).flatMap(([file, source]) => renderTimeRefWrites(file, source));
        expect(writes).toEqual([]);
    });

    it('writes each latest-value ref in a layout effect', () => {
        const missing = Object.entries(LATEST_VALUE_REFS).flatMap(([file, refs]) => {
            expect(SOURCES[file], file).toBeDefined();
            const synced = new Set(refWrites(file, SOURCES[file]).filter(w => w.inLayoutEffect).map(w => w.ref));
            return refs.filter(ref => !synced.has(ref)).map(ref => `${file} ${ref}`);
        });
        expect(missing).toEqual([]);
    });

    it('finds a write during render, and leaves callbacks, effects and lazy initialisation alone', () => {
        const source = [
            'function Panel({ onClose, items }) {',
            '  const closeRef = useRef(onClose);',
            '  closeRef.current = onClose;',
            '  const lazy = useRef(null);',
            '  if (lazy.current === null) lazy.current = new Map();',
            '  useLayoutEffect(() => { closeRef.current = onClose; });',
            '  const onClick = () => { closeRef.current = null; };',
            '  const countRef = useRef(0);',
            '  countRef.current++;',
            '  countRef.current += 1;',
            "  closeRef['current'] = onClose;",
            '  const rows = useMemo(() => { closeRef.current = onClose; return items; }, [onClose, items]);',
            '  const [first] = useState(() => { countRef.current = 1; return 0; });',
            '  items.forEach(item => { closeRef.current = item; });',
            '  (() => { countRef.current = 2; })();',
            '  if (lazy.current === null) countRef.current = 3;',
            '  if (other_lazy.current === null) lazy.current = new Map();',
            '  if (lazy.current === null) { } else { lazy.current = new Map(); }',
            '  if (!lazy.current) lazy.current = new Map();',
            '  if (null == lazy.current) { lazy.current = new Map(); }',
            '  lazy.current ??= new Map();',
            '  const bump = useCallback(() => { countRef.current++; countRef.current += 1; }, []);',
            '  const later = useCallback(() => items.map(item => { closeRef.current = item; }), [items]);',
            '  return null;',
            '}',
            'const useLatest = (value) => { const r = useRef(value); r.current = value; return r; };',
            'const View = memo(() => { const r = useRef(0); r.current = 1; return null; });',
        ].join('\n');
        expect(renderTimeRefWrites('/src/x.tsx', source)).toEqual([
            '/src/x.tsx:3 Panel: closeRef.current = onClose',
            '/src/x.tsx:9 Panel: countRef.current++',
            '/src/x.tsx:10 Panel: countRef.current += 1',
            "/src/x.tsx:11 Panel: closeRef['current'] = onClose",
            '/src/x.tsx:12 Panel: closeRef.current = onClose',
            '/src/x.tsx:13 Panel: countRef.current = 1',
            '/src/x.tsx:14 Panel: closeRef.current = item',
            '/src/x.tsx:15 Panel: countRef.current = 2',
            '/src/x.tsx:16 Panel: countRef.current = 3',
            '/src/x.tsx:17 Panel: lazy.current = new Map()',
            '/src/x.tsx:18 Panel: lazy.current = new Map()',
            '/src/x.tsx:26 useLatest: r.current = value',
            '/src/x.tsx:27 View: r.current = 1',
        ]);
        expect(refWrites('/src/x.tsx', source).filter(w => w.inLayoutEffect).map(w => `${w.line} ${w.ref}`)).toEqual(['6 closeRef']);
    });
});
