// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';
import libRs from '../../src-tauri/src/lib.rs?raw';

/**
 * Every Tauri command registered in `lib.rs` has a caller in the frontend.
 *
 * A registered command is IPC surface: anything running in the webview can
 * invoke it, whether or not the app ever does. A command nobody calls costs
 * that surface and buys nothing, and it rots silently because no flow
 * exercises it. Seven `user_partitions_*` commands were found in that state,
 * one of which handed a decrypted per-user credential to the webview as a
 * plain string; all of them were superseded by flows that never needed them.
 *
 * "Has a caller" follows one hop. A command name that appears only inside a
 * wrapper (`export async function getActiveUser() { invoke(...) }`) is not
 * called by being wrapped: four of the seven had a typed wrapper that nothing
 * imported. So a literal inside a named top-level declaration counts only when
 * that name is used: referenced elsewhere in its own file, or, when the file
 * exports it (inline or through an `export { ... }` list), imported under that
 * name by another file that then references the import. An alias counts; an
 * import nobody references does not, and neither does the export list. Each
 * declarator of `const a = ..., b = ...` is a declaration of its own. Names are
 * resolved through scopes, so a parameter, a local, a property key or a member
 * name spelled like the wrapper is not a use of it. An import is matched to the
 * module its specifier resolves to (the path, `.ts`, `.tsx` or the directory
 * `index`, through `export ... from` re-exports), never to every export of the
 * same name: another module's `start` is not a use of this one, and an import
 * from a package or a module the scan does not read matches nothing. A literal
 * outside any named declaration (a top-level call, an anonymous default export)
 * counts as a caller as it stands.
 *
 * A hook, or any named top-level function whose every return is an object
 * literal, is followed to the member: code that runs only when a member is
 * called counts only when a caller reads that member, by destructuring it from
 * the call or as `.member` on the result (one alias deep). A result handed on
 * whole (a prop, a spread, a return) reads every member.
 *
 * And only a literal that reaches an invoke-like call as the command counts
 * (`reachesInvoke`): a command name in a log line, a comparison or a label is
 * not a call.
 *
 * `INHERITED_UNCALLED` is the set that already had no caller when this guard
 * landed and has not been audited yet. It may only shrink: an entry that gains
 * a caller or is unregistered fails the last test until it is removed here.
 */

const sources = import.meta.glob('../**/*.{ts,tsx}', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>;

/**
 * Command names from the `tauri::generate_handler![` block, parsed the way
 * `src-tauri/build.rs` parses it for the inventory: last path segment is the
 * name, attributes and comments skipped. `cargo fmt` keeps one path per line;
 * a line holding several is split on commas rather than read as one name, and
 * every name is checked to be an identifier by the first test below.
 */
function registeredCommands(lib: string): string[] {
    const lines = lib.split('\n');
    const start = lines.findIndex((l) => l.includes('generate_handler!['));
    const names: string[] = [];
    if (start < 0) return names;
    for (const raw of lines.slice(start + 1)) {
        const line = raw.trim();
        if (line.startsWith('])')) break;
        if (line === '' || line.startsWith('#[') || line.startsWith('//')) continue;
        const end = line.indexOf('])');
        for (const part of (end >= 0 ? line.slice(0, end) : line).split(',')) {
            const token = part.trim();
            if (token === '' || token.startsWith('//')) continue;
            names.push(token.split('::').pop() ?? token);
        }
        if (end >= 0) break;
    }
    return names;
}

// The scan reads the AST, not the text. A regex that blanks comments cannot
// tell a `/**` inside a line comment from the start of a block comment, and the
// first version of this guard lost a hundred lines of `useUiTokens.ts` to that.
type Node = { type: string; start: number; end: number; [key: string]: unknown };

const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && 'type' in value;

/** Keys that hold types: nothing under them runs, so nothing there calls a command or uses a binding. */
const TYPE_KEYS = new Set([
    'typeAnnotation', 'returnType', 'typeParameters', 'typeArguments', 'superTypeArguments', 'superTypeParameters', 'implements',
]);

/**
 * Declarations that exist only for the type checker. Enums are values, but no
 * command name lives in one, and their member names are not references.
 */
const TYPE_ONLY = new Set([
    'TSInterfaceDeclaration', 'TSTypeAliasDeclaration', 'TSDeclareFunction', 'TSModuleDeclaration', 'TSEnumDeclaration',
    'TSImportEqualsDeclaration', 'TSIndexSignature', 'TSNamespaceExportDeclaration',
]);

const FUNCTION_TYPES = new Set(['FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression']);

/** Wrappers that hand their operand's value on unchanged. */
const TRANSPARENT = new Set([
    'TSAsExpression', 'TSSatisfiesExpression', 'TSNonNullExpression', 'TSTypeAssertion', 'TSInstantiationExpression',
    'AwaitExpression', 'ChainExpression', 'ParenthesizedExpression',
]);

function children(node: Node): Array<[string, Node]> {
    const out: Array<[string, Node]> = [];
    for (const [key, value] of Object.entries(node)) {
        if (TYPE_KEYS.has(key)) continue;
        if (Array.isArray(value)) {
            for (const item of value) if (isNode(item)) out.push([key, item]);
        } else if (isNode(value)) {
            out.push([key, value]);
        }
    }
    return out;
}

function unwrap(node: Node): Node {
    while (TRANSPARENT.has(node.type)) node = (node.expression ?? node.argument) as Node;
    return node;
}

/** The string a literal node spells, for plain strings and substitution-free templates. */
function stringValue(node: Node): string | null {
    if (node.type === 'Literal' && typeof node.value === 'string') return node.value;
    if (node.type === 'TemplateLiteral' && (node.expressions as unknown[]).length === 0) {
        const quasis = node.quasis as Array<{ value: { cooked: string | null } }>;
        return quasis[0]?.value.cooked ?? null;
    }
    return null;
}

/** The name a non-computed property, method or class member key spells, or null. */
function keyName(node: Node): string | null {
    if (node.computed) return null;
    const key = node.key as Node;
    return key.type === 'Identifier' ? (key.name as string) : stringValue(key);
}

/** The identifiers a binding pattern declares. */
function patternIds(pattern: Node | null | undefined): Node[] {
    if (!pattern) return [];
    switch (pattern.type) {
        case 'Identifier':
            return [pattern];
        case 'ObjectPattern':
            return (pattern.properties as Node[]).flatMap((p) => patternIds((p.type === 'RestElement' ? p.argument : p.value) as Node));
        case 'ArrayPattern':
            return (pattern.elements as Array<Node | null>).flatMap((e) => patternIds(e));
        case 'AssignmentPattern':
            return patternIds(pattern.left as Node);
        case 'RestElement':
            return patternIds(pattern.argument as Node);
        case 'TSParameterProperty':
            return patternIds(pattern.parameter as Node);
        default:
            return [];
    }
}

const namesOf = (pattern: Node | null | undefined): string[] => patternIds(pattern).map((id) => id.name as string);

/** The names an assignment writes: a variable, a property, or the names of a destructuring target. */
function targetNames(left: Node): string[] {
    if (left.type === 'MemberExpression') return left.computed ? [] : [(left.property as Node).name as string];
    return namesOf(left);
}

/** The declarations a statement list hoists into its scope, each with the node that declares it. */
function declarations(statements: Node[]): Array<{ id: Node; decl: Node }> {
    const out: Array<{ id: Node; decl: Node }> = [];
    for (const statement of statements) {
        const isExport = statement.type === 'ExportNamedDeclaration' || statement.type === 'ExportDefaultDeclaration';
        const decl = (isExport ? statement.declaration : statement) as Node | null;
        if (!decl) continue;
        if (decl.type === 'VariableDeclaration') {
            for (const declarator of decl.declarations as Node[]) {
                for (const id of patternIds(declarator.id as Node)) out.push({ id, decl: declarator });
            }
        } else if ((decl.type === 'FunctionDeclaration' || decl.type === 'ClassDeclaration') && decl.id) {
            out.push({ id: decl.id as Node, decl });
        }
    }
    return out;
}

/** Every identifier name in an expression, nested functions excluded (what they return flows by their own name). */
function namesIn(node: Node | null | undefined, out = new Set<string>()): Set<string> {
    if (!node || FUNCTION_TYPES.has(node.type)) return out;
    if (node.type === 'Identifier' || node.type === 'JSXIdentifier') out.add(node.name as string);
    for (const [, child] of children(node)) namesIn(child, out);
    return out;
}

/** Local name -> the names other files can import it by. */
function exportedNames(program: Node): Map<string, Set<string>> {
    const exported = new Map<string, Set<string>>();
    const add = (local: string, name: string) => exported.set(local, (exported.get(local) ?? new Set()).add(name));
    const spelled = (node: Node) => (node.name ?? node.value) as string;
    for (const statement of program.body as Node[]) {
        if (statement.type === 'ExportNamedDeclaration') {
            for (const { id } of declarations([statement])) add(id.name as string, id.name as string);
            // `export { a, b as c }` without `from`: names declared in this file.
            if (!statement.source) {
                for (const spec of statement.specifiers as Node[]) {
                    const local = spelled(spec.local as Node);
                    const name = spelled(spec.exported as Node);
                    // A default export is imported under whatever name the importer picks;
                    // the guard matches it by the name it is declared with.
                    add(local, name === 'default' ? local : name);
                }
            }
        } else if (statement.type === 'ExportDefaultDeclaration') {
            const decl = statement.declaration as Node;
            const id = (decl.type === 'Identifier' ? decl : decl.id) as Node | null;
            if (id) add(id.name as string, id.name as string);
        }
    }
    return exported;
}

/** Hook members read: a set of member names, or null when any member may be read. */
type Reads = Set<string> | null;

const mergeReads = (a: Reads, b: Reads): Reads => (a === null || b === null ? null : new Set([...a, ...b]));

interface Binding {
    name: string;
    /** The node that declares it: a declarator, a function, a class, a catch clause, an import. */
    decl: Node;
    /** For a binding an import makes (static, or destructured from `await import()`): the name imported. */
    imported: string | null;
    /** The module specifier it was imported from, when it is a literal. */
    source: string | null;
    /** The identifiers that resolve to it. */
    refs: Node[];
}

interface Scope {
    parent: Scope | null;
    bindings: Map<string, Binding>;
}

interface Literal {
    value: string;
    node: Node;
    /** The named top-level declaration it sits in, or null (top-level code, anonymous default export). */
    owner: string | null;
    /** When the owner is a function returning an object literal: the members whose (deferred) code holds it. */
    members: Set<string> | null;
}

interface ScannedFile {
    file: string;
    parents: Map<Node, Node>;
    /** Every string literal in code that runs (types excluded). */
    literals: Literal[];
    calls: Array<{ node: Node; callee: string | null; imported: ImportRef | null; member: string | null }>;
    /** Named functions with the name of each simple parameter (null for a destructured one). */
    functions: Array<{ name: string; params: Array<string | null>; exported: string[] }>;
    /** Name-level value flow: whatever the sources name may end up in any of the targets. */
    flows: Array<{ targets: string[]; sources: Set<string> }>;
    /** Function node -> the name it is called by. */
    functionNames: Map<Node, string | null>;
    exported: Map<string, Set<string>>;
    /** Local name -> what it was imported as. */
    imports: Map<string, ImportRef>;
    /** Top-level name -> members read by the uses outside its own declaration. Absent: unused here. */
    localUses: Map<string, Reads>;
    /** Each import referenced here, once per module and name, with the members read through it. */
    importUses: Array<{ ref: ImportRef; reads: Reads }>;
    /** `export { a as b } from './x'` and `export * from './x'` (imported and exported '*'). */
    reexports: Array<{ source: string; imported: string; exported: string }>;
}

/** An imported name and the module specifier it comes from (null: not a literal). */
interface ImportRef {
    name: string;
    source: string | null;
}

function scan(file: string, source: string): ScannedFile {
    const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
    const parents = new Map<Node, Node>();
    const bindingOf = new Map<Node, Binding>();
    const refOf = new Map<Node, Binding>();
    const importBindings: Binding[] = [];
    const strings: Node[] = [];
    const callNodes: Node[] = [];
    const functionNodes: Array<{ node: Node; name: string | null; params: Node[] }> = [];
    const flows: ScannedFile['flows'] = [];
    const functionNames = new Map<Node, string | null>();
    const functionStack: Array<string | null> = [];

    const declare = (scope: Scope, id: Node, decl: Node, imported: string | null = null, source: string | null = null): Binding => {
        const binding: Binding = { name: id.name as string, decl, imported, source, refs: [] };
        scope.bindings.set(binding.name, binding);
        bindingOf.set(id, binding);
        return binding;
    };
    const enter = (parent: Scope | null, decls: Array<{ id: Node; decl: Node }>): Scope => {
        const scope: Scope = { parent, bindings: new Map() };
        for (const { id, decl } of decls) declare(scope, id, decl);
        return scope;
    };
    const reference = (id: Node, scope: Scope) => {
        for (let s: Scope | null = scope; s; s = s.parent) {
            const binding = s.bindings.get(id.name as string);
            if (binding) {
                binding.refs.push(id);
                refOf.set(id, binding);
                return;
            }
        }
    };
    /** The name a function is called by: its declarator, its property key, its own id. */
    const functionName = (fn: Node): string | null => {
        let parent = parents.get(fn);
        // `const run = useCallback((cmd) => ..., [])`: the callback is called as `run`.
        if (parent?.type === 'CallExpression' && parent.callee !== fn) parent = parents.get(parent);
        if (parent?.type === 'VariableDeclarator' && (parent.id as Node).type === 'Identifier') return (parent.id as Node).name as string;
        if (parent && /^(Property|MethodDefinition|PropertyDefinition)$/.test(parent.type) && parent.value === fn) return keyName(parent);
        return ((fn.id as Node | null)?.name as string | undefined) ?? null;
    };

    const visitChild = (parent: Node, node: Node | null | undefined, scope: Scope) => {
        if (!node) return;
        parents.set(node, parent);
        visit(node, scope);
    };
    // A binding pattern declares names (already in scope); only its defaults and computed keys run.
    const visitPattern = (parent: Node, node: Node | null | undefined, scope: Scope): void => {
        if (!node) return;
        parents.set(node, parent);
        switch (node.type) {
            case 'Identifier':
                return;
            case 'ObjectPattern':
                for (const prop of node.properties as Node[]) {
                    parents.set(prop, node);
                    if (prop.type === 'RestElement') {
                        visitPattern(prop, prop.argument as Node, scope);
                    } else {
                        if (prop.computed) visitChild(prop, prop.key as Node, scope);
                        visitPattern(prop, prop.value as Node, scope);
                    }
                }
                return;
            case 'ArrayPattern':
                for (const element of node.elements as Array<Node | null>) visitPattern(node, element, scope);
                return;
            case 'AssignmentPattern':
                visitPattern(node, node.left as Node, scope);
                visitChild(node, node.right as Node, scope);
                return;
            case 'RestElement':
                visitPattern(node, node.argument as Node, scope);
                return;
            case 'TSParameterProperty':
                visitPattern(node, node.parameter as Node, scope);
                return;
            default:
                // An assignment target such as `a.b`.
                visit(node, scope);
        }
    };
    const visitJsxName = (parent: Node, name: Node, scope: Scope, root: boolean) => {
        parents.set(name, parent);
        // `<div>` is an intrinsic element; `<Foo>` and the `motion` of `<motion.div>` are bindings.
        if (name.type === 'JSXIdentifier' && (!root || /^[A-Z]/.test(name.name as string))) reference(name, scope);
        else if (name.type === 'JSXMemberExpression') visitJsxName(name, name.object as Node, scope, false);
    };

    const visit = (node: Node, scope: Scope): void => {
        if (TYPE_ONLY.has(node.type)) return;
        switch (node.type) {
            case 'Identifier':
                reference(node, scope);
                return;
            case 'Literal':
            case 'TemplateLiteral':
                if (stringValue(node) !== null) strings.push(node);
                break;
            case 'ImportDeclaration':
            case 'ExportAllDeclaration':
            case 'MetaProperty':
            case 'BreakStatement':
            case 'ContinueStatement':
            case 'JSXClosingElement':
                return;
            case 'ExportNamedDeclaration':
                // `export { unused }` names a binding, it does not use it.
                visitChild(node, node.declaration as Node | null, scope);
                return;
            case 'ExportDefaultDeclaration':
                // `export default Foo` exports Foo; it is not a use of it either.
                if ((node.declaration as Node).type !== 'Identifier') visitChild(node, node.declaration as Node, scope);
                return;
            case 'VariableDeclarator': {
                visitPattern(node, node.id as Node, scope);
                visitChild(node, node.init as Node | null, scope);
                const init = node.init ? unwrap(node.init as Node) : null;
                if (init) flows.push({ targets: namesOf(node.id as Node), sources: namesIn(init) });
                // `const { a } = await import('./x')` imports `a` as surely as a static import does.
                if (init?.type === 'ImportExpression' && (node.id as Node).type === 'ObjectPattern') {
                    for (const prop of (node.id as Node).properties as Node[]) {
                        const binding = prop.type === 'Property' ? bindingOf.get(patternIds(prop.value as Node)[0]) : undefined;
                        const name = prop.type === 'Property' ? keyName(prop) : null;
                        if (binding && name !== null) {
                            binding.imported = name;
                            binding.source = stringValue(init.source as Node);
                            importBindings.push(binding);
                        }
                    }
                }
                return;
            }
            case 'FunctionDeclaration':
            case 'FunctionExpression':
            case 'ArrowFunctionExpression': {
                const name = functionName(node);
                functionNames.set(node, name);
                const params = node.params as Node[];
                const inner = enter(scope, params.flatMap((p) => patternIds(p).map((id) => ({ id, decl: node }))));
                if (node.type === 'FunctionExpression' && node.id) declare(inner, node.id as Node, node);
                functionNodes.push({ node, name, params });
                functionStack.push(name);
                for (const param of params) visitPattern(node, param, inner);
                const body = node.body as Node;
                visitChild(node, body, inner);
                if (body.type !== 'BlockStatement' && name !== null) flows.push({ targets: [name], sources: namesIn(unwrap(body)) });
                functionStack.pop();
                return;
            }
            case 'ClassDeclaration':
            case 'ClassExpression':
                visitChild(node, node.superClass as Node | null, scope);
                visitChild(node, node.body as Node, scope);
                return;
            case 'BlockStatement':
            case 'StaticBlock': {
                const inner = enter(scope, declarations(node.body as Node[]));
                for (const statement of node.body as Node[]) visitChild(node, statement, inner);
                return;
            }
            case 'SwitchStatement': {
                visitChild(node, node.discriminant as Node, scope);
                const cases = node.cases as Node[];
                const inner = enter(scope, declarations(cases.flatMap((c) => c.consequent as Node[])));
                for (const c of cases) visitChild(node, c, inner);
                return;
            }
            case 'ForStatement':
            case 'ForInStatement':
            case 'ForOfStatement': {
                const head = (node.init ?? node.left) as Node | null;
                const declared = head?.type === 'VariableDeclaration' ? declarations([head]) : [];
                const inner = enter(scope, declared);
                for (const [, child] of children(node)) visitChild(node, child, inner);
                if (node.right && head) {
                    const target = head.type === 'VariableDeclaration' ? ((head.declarations as Node[])[0].id as Node) : head;
                    flows.push({ targets: targetNames(target), sources: namesIn(node.right as Node) });
                }
                return;
            }
            case 'CatchClause': {
                const inner = enter(scope, patternIds(node.param as Node | null).map((id) => ({ id, decl: node })));
                visitPattern(node, node.param as Node | null, inner);
                visitChild(node, node.body as Node, inner);
                return;
            }
            case 'AssignmentExpression':
                visitPattern(node, node.left as Node, scope);
                visitChild(node, node.right as Node, scope);
                flows.push({ targets: targetNames(node.left as Node), sources: namesIn(node.right as Node) });
                return;
            case 'ReturnStatement': {
                visitChild(node, node.argument as Node | null, scope);
                const name = functionStack[functionStack.length - 1];
                if (name && node.argument) flows.push({ targets: [name], sources: namesIn(node.argument as Node) });
                return;
            }
            case 'CallExpression':
                callNodes.push(node);
                break;
            case 'MemberExpression':
                visitChild(node, node.object as Node, scope);
                if (node.computed) visitChild(node, node.property as Node, scope);
                return;
            case 'LabeledStatement':
                visitChild(node, node.body as Node, scope);
                return;
            case 'JSXOpeningElement':
                visitJsxName(node, node.name as Node, scope, true);
                for (const attribute of node.attributes as Node[]) visitChild(node, attribute, scope);
                return;
            case 'JSXAttribute':
                visitChild(node, node.value as Node | null, scope);
                return;
        }
        for (const [key, child] of children(node)) {
            // Property, method and class member names are not references.
            if (key === 'key' && !node.computed) continue;
            visitChild(node, child, scope);
        }
    };

    const body = program.body as Node[];
    const moduleScope = enter(null, declarations(body));
    for (const statement of body) {
        if (statement.type !== 'ImportDeclaration') continue;
        for (const spec of statement.specifiers as Node[]) {
            const imported = spec.imported as Node | undefined;
            // A default import is matched by its local name, the way default exports are by their declared one.
            const local = (spec.local as Node).name as string;
            const name = spec.type === 'ImportNamespaceSpecifier' ? '*' : imported ? ((imported.name ?? imported.value) as string) : local;
            importBindings.push(declare(moduleScope, spec.local as Node, statement, name, stringValue(statement.source as Node)));
        }
    }
    for (const statement of body) visitChild(program, statement, moduleScope);

    // Owners: each top-level declarator, function or class owns the literals inside it.
    const roots = new Map<Node, string | null>();
    for (const statement of body) {
        const isExport = statement.type === 'ExportNamedDeclaration' || statement.type === 'ExportDefaultDeclaration';
        const decl = (isExport ? statement.declaration : statement) as Node | null;
        if (decl?.type === 'VariableDeclaration') {
            for (const declarator of decl.declarations as Node[]) {
                const id = declarator.id as Node;
                roots.set(declarator, id.type === 'Identifier' ? (id.name as string) : null);
            }
        } else if (decl && (decl.type === 'FunctionDeclaration' || decl.type === 'ClassDeclaration') && decl.id) {
            roots.set(decl, (decl.id as Node).name as string);
        } else {
            roots.set(statement, null);
        }
    }

    // Members: a named top-level function whose every return is an object literal hands
    // out its members one by one. Code that runs only when a member is called (a function
    // in the member's value, or a local declared in the body and referenced nowhere but
    // in the returned objects) belongs to that member.
    const memberRoots = new Map<Node, Set<string>>();
    for (const [root, name] of roots) {
        const fn = (root.type === 'VariableDeclarator' ? root.init : root) as Node | null;
        if (name === null || !fn || !FUNCTION_TYPES.has(fn.type)) continue;
        const objects = returnedObjects(fn);
        if (!objects) continue;
        const props = objects.flatMap((o) => (o.properties as Node[]).filter((p) => p.type === 'Property'));
        const values = new Set(props.map((p) => unwrap(p.value as Node)));
        const isBodyLocal = (binding: Binding) => {
            const holder = binding.decl.type === 'VariableDeclarator' ? parents.get(binding.decl) : binding.decl;
            return !!holder && parents.get(holder) === fn.body;
        };
        for (const object of objects) {
            for (const prop of object.properties as Node[]) {
                const key = prop.type === 'Property' ? keyName(prop) : null;
                if (key === null) continue;
                const value = unwrap(prop.value as Node);
                const local = value.type === 'Identifier' ? refOf.get(value) : undefined;
                const target = !local ? value : isBodyLocal(local) && local.refs.every((r) => values.has(r)) ? local.decl : null;
                if (target) memberRoots.set(target, (memberRoots.get(target) ?? new Set()).add(key));
            }
        }
    }

    const literals: Literal[] = strings.map((node) => {
        let owner: string | null = null;
        let members: Set<string> | null = null;
        let deferred = false;
        for (let n: Node | undefined = node; n; n = parents.get(n)) {
            if (FUNCTION_TYPES.has(n.type)) deferred = true;
            const keys = memberRoots.get(n);
            if (keys && deferred && members === null) members = keys;
            if (roots.has(n)) {
                owner = roots.get(n) ?? null;
                break;
            }
        }
        return { value: stringValue(node) as string, node, owner, members };
    });

    /** Members read off the value `node` evaluates to. */
    const membersRead = (node: Node, depth: number): Reads => {
        let n = node;
        let parent = parents.get(n);
        while (parent && TRANSPARENT.has(parent.type)) {
            n = parent;
            parent = parents.get(n);
        }
        if (!parent) return null;
        if (parent.type === 'ExpressionStatement') return new Set();
        if (parent.type === 'MemberExpression' && parent.object === n) {
            return parent.computed ? null : new Set([(parent.property as Node).name as string]);
        }
        if (parent.type === 'VariableDeclarator' && parent.init === n) {
            const id = parent.id as Node;
            if (id.type === 'ObjectPattern') {
                const keys = (id.properties as Node[]).map((p) => (p.type === 'Property' ? keyName(p) : null));
                return keys.every((k) => k !== null) ? new Set(keys as string[]) : null;
            }
            // `const oauth = useOAuth(); oauth.logout()`: one alias deep, read through its uses.
            const alias = id.type === 'Identifier' && depth === 0 ? bindingOf.get(id) : undefined;
            if (alias) return alias.refs.reduce<Reads>((reads, ref) => mergeReads(reads, membersRead(ref, 1)), new Set());
        }
        return null;
    };
    /** Members read through references: a reference that calls the binding reads what the result is used for. */
    const readsThrough = (refs: Node[]): Reads =>
        refs.reduce<Reads>((reads, ref) => {
            const parent = parents.get(ref);
            return mergeReads(reads, parent?.type === 'CallExpression' && parent.callee === ref ? membersRead(parent, 0) : null);
        }, new Set());

    const localUses = new Map<string, Reads>();
    for (const binding of moduleScope.bindings.values()) {
        if (binding.imported !== null) continue;
        const { start, end } = binding.decl;
        // A reference inside the declaration itself (recursion) is not a use from outside.
        const refs = binding.refs.filter((r) => r.start < start || r.start >= end);
        if (refs.length > 0) localUses.set(binding.name, readsThrough(refs));
    }
    const uses = new Map<string, { ref: ImportRef; reads: Reads }>();
    const addImportUse = (name: string, source: string | null, reads: Reads) => {
        const key = `${source}\n${name}`;
        const seen = uses.get(key);
        uses.set(key, { ref: { name, source }, reads: seen ? mergeReads(seen.reads, reads) : reads });
    };
    for (const binding of importBindings) {
        if (binding.imported === '*') {
            for (const ref of binding.refs) {
                const parent = parents.get(ref);
                if (parent?.type !== 'MemberExpression' || parent.object !== ref || parent.computed) continue;
                addImportUse((parent.property as Node).name as string, binding.source, null);
            }
        } else if (binding.refs.length > 0) {
            addImportUse(binding.imported as string, binding.source, readsThrough(binding.refs));
        }
    }
    const importUses = [...uses.values()];
    const reexports: ScannedFile['reexports'] = [];
    for (const statement of body) {
        const source = statement.source ? stringValue(statement.source as Node) : null;
        if (source === null) continue;
        if (statement.type === 'ExportAllDeclaration' && !statement.exported) {
            reexports.push({ source, imported: '*', exported: '*' });
        } else if (statement.type === 'ExportNamedDeclaration') {
            for (const spec of statement.specifiers as Node[]) {
                const local = spec.local as Node;
                const exported = spec.exported as Node;
                reexports.push({ source, imported: (local.name ?? local.value) as string, exported: (exported.name ?? exported.value) as string });
            }
        }
    }

    const exported = exportedNames(program);
    const calls = callNodes.map((node) => {
        const callee = unwrap(node.callee as Node);
        if (callee.type === 'Identifier') {
            const binding = refOf.get(callee);
            const imported = binding?.imported ? { name: binding.imported, source: binding.source } : null;
            return { node, callee: callee.name as string, imported, member: null };
        }
        const member = callee.type === 'MemberExpression' && !callee.computed ? ((callee.property as Node).name as string) : null;
        return { node, callee: null, imported: null, member };
    });
    const functions = functionNodes.flatMap(({ node, name, params }) => {
        if (name === null) return [];
        const parent = parents.get(node);
        const wrapped = parent?.type === 'CallExpression' ? parents.get(parent) : undefined;
        const isTopLevel = roots.has(node) || (!!parent && roots.has(parent)) || (!!wrapped && roots.has(wrapped));
        const simple = params.map((p) => {
            const id = p.type === 'AssignmentPattern' ? (p.left as Node) : p;
            return id.type === 'Identifier' ? (id.name as string) : null;
        });
        return [{ name, params: simple, exported: isTopLevel ? [...(exported.get(name) ?? [])] : [] }];
    });
    const imports = new Map(importBindings.map((b) => [b.name, { name: b.imported as string, source: b.source }]));
    return { file, parents, literals, calls, functions, flows, functionNames, exported, imports, localUses, importUses, reexports };
}

/** The object literals a function returns, when every one of its own returns is one; else null. */
function returnedObjects(fn: Node): Node[] | null {
    const body = fn.body as Node;
    const returned: Array<Node | null> = [];
    if (body.type === 'BlockStatement') {
        const walk = (node: Node) => {
            if (node.type === 'ReturnStatement') returned.push(node.argument as Node | null);
            for (const [, child] of children(node)) if (!FUNCTION_TYPES.has(child.type) && child.type !== 'ClassBody') walk(child);
        };
        walk(body);
    } else {
        returned.push(body);
    }
    const objects = returned.map((r) => (r ? unwrap(r) : null));
    return objects.length > 0 && objects.every((o) => o?.type === 'ObjectExpression') ? (objects as Node[]) : null;
}

/** Test and type files are not callers: only code the app ships can invoke a command. */
function isAppSource(file: string): boolean {
    return !/\.(?:test|spec)\.tsx?$/.test(file) && !file.includes('/__tests__/') && !file.endsWith('.d.ts');
}

/** Glob keys are relative to this directory (`./x.ts` here, `../x.ts` above); files are named from the repository root. */
const repoPath = (key: string): string => (key.startsWith('../') ? `src/${key.slice(3)}` : `src/utils/${key.slice(2)}`);

const appFiles = Object.entries(sources)
    .filter(([file]) => isAppSource(file))
    .map(([file, source]) => scan(repoPath(file), source));

/** A specifier with an extension other than a script's names an asset (JSON, CSS, an image), not a module the scan reads. */
const isAsset = (source: string): boolean => /\.(?!(?:[cm]?[jt]sx?)$)[a-z0-9]+(?:\?\w+)?$/i.test(source);

/**
 * The scanned file a relative specifier names, from the importing file: the
 * path itself, with `.ts` or `.tsx`, or its directory `index`. Null when none
 * is scanned. A bare specifier (a package) is never app code.
 */
function resolveModule(from: string, source: string, files: Set<string>): string | null {
    if (!source.startsWith('.')) return null;
    const parts = from.split('/').slice(0, -1);
    for (const part of source.replace(/\.[cm]?jsx?$/, '').split('/')) {
        if (part === '.' || part === '') continue;
        if (part === '..' && parts.length > 0 && parts[parts.length - 1] !== '..') parts.pop();
        else parts.push(part);
    }
    const base = parts.join('/');
    return [base, `${base}.ts`, `${base}.tsx`, `${base}/index.ts`, `${base}/index.tsx`].find((c) => files.has(c)) ?? null;
}

/** An exported name of a scanned file: where an import ends up once re-exports are followed. */
interface Origin {
    file: ScannedFile;
    name: string;
}

const originKey = (origin: Origin) => `${origin.file.file}#${origin.name}`;

interface Analysis {
    /** Invoke-like call -> the argument positions that name the command. */
    commandArgs: Map<Node, Set<number>>;
    /** Per file: every name a command name can travel through on its way into an invoke-like call. */
    carrying: Map<ScannedFile, Set<string>>;
    /** Exports (by origin key) that carry a command into an invoke-like call in a file importing them. */
    carryingExports: Set<string>;
    /** The exports an import in `importer` reaches: empty for a package or a module the scan does not read. */
    originsOf: (importer: ScannedFile, ref: ImportRef) => Origin[];
    byValue: Map<string, Array<{ scanned: ScannedFile; literal: Literal }>>;
}

/** Follows re-exports (`export { a as b } from`, `export * from`) to the files that declare a name. */
function originResolver(files: ScannedFile[]): Analysis['originsOf'] {
    const byFile = new Map(files.map((f) => [f.file, f]));
    const names = new Set(byFile.keys());
    const provides = (file: ScannedFile, name: string, seen: Set<string>): Origin[] => {
        const key = `${file.file}#${name}`;
        if (seen.has(key)) return [];
        seen.add(key);
        const out: Origin[] = [...file.exported.values()].some((n) => n.has(name)) ? [{ file, name }] : [];
        for (const reexport of file.reexports) {
            const star = reexport.exported === '*';
            if (star ? name === 'default' : reexport.exported !== name) continue;
            const target = resolveModule(file.file, reexport.source, names);
            if (target !== null) out.push(...provides(byFile.get(target) as ScannedFile, star ? name : reexport.imported, seen));
        }
        return out;
    };
    return (importer, ref) => {
        const target = ref.source === null ? null : resolveModule(importer.file, ref.source, names);
        return target === null ? [] : provides(byFile.get(target) as ScannedFile, ref.name, new Set());
    };
}

const analyses = new WeakMap<ScannedFile[], Analysis>();

/**
 * Which calls are invoke-like, and which names carry a command into one.
 *
 * `invoke(...)` and `x.invoke(...)` take the command first. A function whose
 * parameter carries a command into an invoke-like call is invoke-like at that
 * position too (`runAction('github_rerun_workflow', id)`), in its own file by
 * its local name and elsewhere by its exported one; that is iterated to a fixed
 * point, since forwarders forward to forwarders.
 */
function analyze(files: ScannedFile[]): Analysis {
    const cached = analyses.get(files);
    if (cached) return cached;
    const originsOf = originResolver(files);
    const forwardingByFile = new Map<ScannedFile, Map<string, Set<number>>>(files.map((f) => [f, new Map()]));
    /** Origin key of an exported forwarder -> the positions it forwards. */
    const forwardingByExport = new Map<string, Set<number>>();
    const forwardedThrough = (scanned: ScannedFile, ref: ImportRef): Set<number> | undefined => {
        const positions = originsOf(scanned, ref).flatMap((o) => [...(forwardingByExport.get(originKey(o)) ?? [])]);
        return positions.length > 0 ? new Set(positions) : undefined;
    };
    const commandArgs = new Map<Node, Set<number>>();
    const carrying = new Map<ScannedFile, Set<string>>();
    const addPosition = (map: Map<string, Set<number>>, name: string, position: number): boolean => {
        const positions = map.get(name) ?? new Set<number>();
        if (positions.has(position)) return false;
        map.set(name, positions.add(position));
        return true;
    };
    const first = new Set([0]);
    for (let changed = true; changed; ) {
        changed = false;
        for (const scanned of files) {
            const forwarding = forwardingByFile.get(scanned) as Map<string, Set<number>>;
            const names = new Set<string>();
            for (const call of scanned.calls) {
                const positions =
                    call.callee === 'invoke' || call.imported?.name === 'invoke' || call.member === 'invoke'
                        ? first
                        : ((call.imported !== null ? forwardedThrough(scanned, call.imported) : undefined) ??
                          (call.callee !== null ? forwarding.get(call.callee) : undefined));
                if (!positions) continue;
                commandArgs.set(call.node, positions);
                for (const position of positions) namesIn((call.node.arguments as Node[])[position], names);
            }
            for (let grew = true; grew; ) {
                grew = false;
                for (const flow of scanned.flows) {
                    if (!flow.targets.some((t) => names.has(t))) continue;
                    for (const source of flow.sources) {
                        if (names.has(source)) continue;
                        names.add(source);
                        grew = true;
                    }
                }
            }
            carrying.set(scanned, names);
            for (const fn of scanned.functions) {
                fn.params.forEach((param, position) => {
                    if (param === null || !names.has(param)) return;
                    if (addPosition(forwarding, fn.name, position)) changed = true;
                    for (const name of fn.exported) {
                        if (addPosition(forwardingByExport, originKey({ file: scanned, name }), position)) changed = true;
                    }
                });
            }
        }
    }
    const carryingExports = new Set<string>();
    for (const scanned of files) {
        for (const name of carrying.get(scanned) as Set<string>) {
            const imported = scanned.imports.get(name);
            if (imported !== undefined) for (const origin of originsOf(scanned, imported)) carryingExports.add(originKey(origin));
        }
    }
    const byValue = new Map<string, Array<{ scanned: ScannedFile; literal: Literal }>>();
    for (const scanned of files) {
        for (const literal of scanned.literals) {
            const entries = byValue.get(literal.value) ?? [];
            if (entries.length === 0) byValue.set(literal.value, entries);
            entries.push({ scanned, literal });
        }
    }
    const analysis = { commandArgs, carrying, carryingExports, originsOf, byValue };
    analyses.set(files, analysis);
    return analysis;
}

/**
 * Whether a literal reaches an invoke-like call as the command: directly as the
 * command argument (through `?:`, `||`, `as` and the like), or stored under a
 * name that carries a command: a variable, an assignment target, a default
 * value, an object key or a function it is returned from, carrying one in its
 * own file, or exported and carrying one in a file that imports it; or passed
 * as a JSX prop that carries one in the component's file. Anything else (a log
 * message, a comparison, a label, an argument of an invoke-like call other than
 * the command) does not call the command it spells.
 */
function reachesInvoke(literal: Literal, scanned: ScannedFile, analysis: Analysis): boolean {
    const carrying = analysis.carrying.get(scanned) as Set<string>;
    const keys: string[] = [];
    const exportCarries = (name: string) =>
        [...(scanned.exported.get(name) ?? [])].some((e) => analysis.carryingExports.has(originKey({ file: scanned, name: e })));
    const flowsIn = (names: string[]) =>
        names.some((n) => carrying.has(n) || exportCarries(n)) || keys.some((k) => carrying.has(k));
    const enclosingName = (node: Node): string | null => {
        for (let n: Node | undefined = node; n; n = scanned.parents.get(n)) {
            if (FUNCTION_TYPES.has(n.type)) return scanned.functionNames.get(n) ?? null;
        }
        return null;
    };
    let node = literal.node;
    for (let parent = scanned.parents.get(node); parent; node = parent, parent = scanned.parents.get(node)) {
        switch (parent.type) {
            case 'CallExpression':
            case 'NewExpression': {
                if (parent.callee === node) return false;
                const positions = analysis.commandArgs.get(parent);
                if (positions) return positions.has((parent.arguments as Node[]).indexOf(node));
                // Any other call hands its argument on (`Object.freeze`, `new Set`, `useState`).
                continue;
            }
            case 'ConditionalExpression':
                if (parent.test === node) return false;
                continue;
            case 'SequenceExpression':
                if ((parent.expressions as Node[]).indexOf(node) !== (parent.expressions as Node[]).length - 1) return false;
                continue;
            case 'Property':
                if (parent.value !== node) return false;
                keys.push(keyName(parent) ?? '');
                continue;
            case 'LogicalExpression':
            case 'ArrayExpression':
            case 'ObjectExpression':
            case 'SpreadElement':
            case 'JSXExpressionContainer':
                continue;
            case 'VariableDeclarator':
                return parent.init === node && flowsIn(namesOf(parent.id as Node));
            case 'AssignmentExpression':
                return parent.right === node && flowsIn(targetNames(parent.left as Node));
            case 'AssignmentPattern':
                return parent.right === node && flowsIn(namesOf(parent.left as Node));
            case 'ReturnStatement':
                return flowsIn([enclosingName(parent) ?? '']);
            case 'ArrowFunctionExpression':
                return parent.body === node && flowsIn([scanned.functionNames.get(parent) ?? '']);
            case 'JSXAttribute': {
                // A prop reaches whatever the component does with it, in the component's own file.
                const element = (scanned.parents.get(parent) as Node).name as Node;
                if (element.type !== 'JSXIdentifier' || !/^[A-Z]/.test(element.name as string)) return false;
                const imported = scanned.imports.get(element.name as string);
                const owners = imported === undefined ? [scanned] : analysis.originsOf(scanned, imported).map((o) => o.file);
                const props = [(parent.name as Node).name as string, ...keys];
                return owners.some((f) => props.some((p) => (analysis.carrying.get(f) as Set<string>).has(p)));
            }
            default:
                if (TRANSPARENT.has(parent.type)) continue;
                return flowsIn([]);
        }
    }
    return false;
}

/** How a named top-level declaration is used from outside itself: undefined when it is not. */
function usesOf(scanned: ScannedFile, name: string, files: ScannedFile[], analysis: Analysis): Reads | undefined {
    let uses: Reads | undefined = scanned.localUses.has(name) ? (scanned.localUses.get(name) as Reads) : undefined;
    const keys = new Set([...(scanned.exported.get(name) ?? [])].map((e) => originKey({ file: scanned, name: e })));
    if (keys.size === 0) return uses;
    for (const other of files) {
        if (other === scanned) continue;
        for (const { ref, reads } of other.importUses) {
            if (!analysis.originsOf(other, ref).some((o) => keys.has(originKey(o)))) continue;
            uses = uses === undefined ? reads : mergeReads(uses, reads);
        }
    }
    return uses;
}

/** Where a command is called from, following one hop through a named wrapper (and the hook member read). */
function callersOf(command: string, files: ScannedFile[] = appFiles): string[] {
    const analysis = analyze(files);
    const callers: string[] = [];
    for (const { scanned, literal } of analysis.byValue.get(command) ?? []) {
        if (!reachesInvoke(literal, scanned, analysis)) continue;
        if (literal.owner === null) {
            callers.push(scanned.file);
            continue;
        }
        const reads = usesOf(scanned, literal.owner, files, analysis);
        if (reads === undefined) continue;
        if (literal.members === null) {
            callers.push(`${scanned.file} via ${literal.owner}`);
            continue;
        }
        const member = [...literal.members].find((m) => reads === null || reads.has(m));
        if (member !== undefined) callers.push(`${scanned.file} via ${literal.owner}.${member}`);
    }
    return callers;
}

/**
 * Registered with no frontend caller when this guard landed (2026-10-02,
 * origin/main 5a7efa640), not yet audited one by one. Each entry is either
 * called by a name the scan cannot see (a computed `invoke(\`${x}_...\`)`) or
 * dead surface waiting for the same treatment the `user_partitions_*` seven got.
 */
const INHERITED_UNCALLED: string[] = [
];

/**
 * Audited and kept without a frontend caller, each with the reason and the
 * owner of the decision. Unlike `INHERITED_UNCALLED` this is not a backlog:
 * every entry was looked at and stays on purpose. It is checked both ways
 * like the inherited list: an entry that gains a caller or is unregistered
 * fails until it is removed here.
 */
const AUDITED_UNCALLED: Record<string, string> = {
    debug_panic_command:
        'Debug builds only (#[cfg(debug_assertions)]): panics on purpose so a developer can check from the ' +
        'devtools console that invoke() rejects instead of hanging (panic_safe.rs). No screen is meant to call it.',
    file_tags_create_label:
        'Custom file-tag labels (beyond the seven Finder-style presets) have had backend create, rename and ' +
        'delete since v2.5.0 but never a screen; useFileTags exposes them unread. Building the label manager ' +
        'or dropping custom labels is an owner decision, tracked in the release tracker.',
    file_tags_delete_label: 'Same decision as file_tags_create_label (custom label manager).',
    file_tags_update_label: 'Same decision as file_tags_create_label (custom label manager).',
    parallel_sync_execute:
        'Parallel FTP sync over transfer_pool.rs. No sync ever called it, and fd10ff6f0 left it in tree for ' +
        'APPENDIX-DAG-ENGINE Fase 2 to adopt or retire: that appendix decides, not this list.',
    transfer_queue_scan_remote_tree:
        'Lazy per-level remote scan built for the staged transfer queue (TQ-2, a0e7a9c1, 884efb56); the panel ' +
        '(TQ-4) shipped without folder expansion. APPENDIX-TRANSFER-QUEUE decides whether it is wired or dropped.',
};

const registered = registeredCommands(libRs);
const uncalled = registered.filter((c) => callersOf(c).length === 0);

describe('registered Tauri commands have a frontend caller', () => {
    it('parses a command list large enough to be the real handler block', () => {
        // A parser that silently matched nothing would make every assertion below pass.
        expect(registered.length).toBeGreaterThan(500);
        expect(appFiles.length).toBeGreaterThan(300);
        // A mis-split line yields a name no command has, which would then read as "uncalled".
        expect(registered.filter((name) => !/^[A-Za-z_][A-Za-z0-9_]*$/.test(name))).toEqual([]);
    });

    it('reads a handler line holding several paths as several commands', () => {
        const lib = 'x.invoke_handler(tauri::generate_handler![\n    a::one, b::two,\n    three])\n';
        expect(registeredCommands(lib)).toEqual(['one', 'two', 'three']);
    });

    it('does not count a wrapper that is only listed in an export block', () => {
        const wrapperFile = scan(
            'src/fake/wrappers.ts',
            "const unused = () => invoke('fake_cmd');\nconst used = () => invoke('fake_used');\nexport { unused, used as renamed };\n",
        );
        const consumer = scan('src/fake/consumer.ts', "import { renamed } from './wrappers';\nrenamed();\n");
        expect(callersOf('fake_cmd', [wrapperFile, consumer])).toEqual([]);
        expect(callersOf('fake_used', [wrapperFile, consumer])).toEqual(['src/fake/wrappers.ts via used']);
    });

    it('does not count a literal parked in a constant nobody reads', () => {
        const file = scan('src/fake/parked.ts', "const PARKED = 'fake_cmd';\n");
        expect(callersOf('fake_cmd', [file])).toEqual([]);
    });

    it('does not count an import nobody references', () => {
        const wrapperFile = scan(
            'src/fake/wrappers.ts',
            "const used = () => invoke('fake_used');\nexport { used as renamed };\n",
        );
        const consumer = scan('src/fake/consumer.ts', "import { renamed } from './wrappers';\nexport const nothing = 0;\n");
        expect(callersOf('fake_used', [wrapperFile, consumer])).toEqual([]);
    });

    it('gives each declarator of a statement its own literals', () => {
        const file = scan(
            'src/fake/multi.ts',
            [
                "const unused = () => invoke('fake_cmd'), other = 0;",
                "const used = () => invoke('fake_used'), more = 1;",
                'export const value = other + more + used();',
            ].join('\n'),
        );
        expect(callersOf('fake_cmd', [file])).toEqual([]);
        expect(callersOf('fake_used', [file])).toEqual(['src/fake/multi.ts via used']);
    });

    it('counts a hook member only when a caller reads that member', () => {
        const hook = scan(
            'src/fake/useOAuth.ts',
            [
                'export function useOAuth() {',
                "    const logout = useCallback(() => invoke('fake_logout'), []);",
                '    return {',
                "        completeAuth: () => invoke('fake_complete'),",
                "        start: () => invoke('fake_start'),",
                "        status: () => invoke('fake_status'),",
                '        logout,',
                '    };',
                '}',
            ].join('\n'),
        );
        const consumer = scan(
            'src/fake/Consumer.tsx',
            [
                "import { useOAuth } from './useOAuth';",
                'export function Consumer() {',
                '    const { start } = useOAuth();',
                '    const oauth = useOAuth();',
                '    return <button onClick={() => { start(); oauth.status(); }} />;',
                '}',
            ].join('\n'),
        );
        const files = [hook, consumer];
        expect(callersOf('fake_complete', files)).toEqual([]);
        expect(callersOf('fake_logout', files)).toEqual([]);
        expect(callersOf('fake_start', files)).toEqual(['src/fake/useOAuth.ts via useOAuth.start']);
        expect(callersOf('fake_status', files)).toEqual(['src/fake/useOAuth.ts via useOAuth.status']);
    });

    it('does not count a command name that never reaches an invoke', () => {
        const file = scan(
            'src/fake/literals.ts',
            [
                "console.log('fake_logged');",
                "const label = 'fake_label';",
                'export const shown = `${label}`;',
                "if (shown === 'fake_compared') console.log(shown);",
                "type Kind = 'fake_typed';",
                "export const kind: Kind = 'fake_typed' as Kind;",
            ].join('\n'),
        );
        expect(callersOf('fake_logged', [file])).toEqual([]);
        expect(callersOf('fake_label', [file])).toEqual([]);
        expect(callersOf('fake_compared', [file])).toEqual([]);
        expect(callersOf('fake_typed', [file])).toEqual([]);
        // A key or prop carrying a command somewhere else does not make every key or prop of that name a caller.
        const runner = scan('src/fake/runner.ts', 'export const run = (tool: { name: string }) => invoke(tool.name);\n');
        const badge = scan('src/fake/Badge.tsx', 'export const Badge = ({ name }: { name: string }) => <span>{name}</span>;\n');
        const tools = scan(
            'src/fake/tools.tsx',
            [
                "import { Badge } from './Badge';",
                "export const TOOLS = [{ name: 'fake_tool' }];",
                'render(<Badge name="fake_badge" />, TOOLS);',
            ].join('\n'),
        );
        expect(callersOf('fake_tool', [runner, badge, tools])).toEqual([]);
        expect(callersOf('fake_badge', [runner, badge, tools])).toEqual([]);
    });

    it('still counts a command routed through a variable, a map, a default, a prop or a forwarding function', () => {
        const extract = scan(
            'src/fake/extract.ts',
            'export async function runExtract(command: string) { return invoke<number>(command); }\n',
        );
        const dialog = scan(
            'src/fake/TagsDialog.tsx',
            [
                "export function TagsDialog({ command = 'fake_box_tags' }: { command?: string }) {",
                '    return <button onClick={() => invoke(command)} />;',
                '}',
            ].join('\n'),
        );
        const routes = scan(
            'src/fake/routes.tsx',
            [
                "import { runExtract } from './extract';",
                "import { TagsDialog } from './TagsDialog';",
                "const EXTRACT: Record<string, string> = { zip: 'fake_zip', tar: 'fake_tar' };",
                'async function run(command: string, id: number) { await invoke(command, { id }); }',
                'function verbFor(kind: string) {',
                "    switch (kind) { case 'a': return 'fake_case_a'; default: return 'fake_case_b'; }",
                '}',
                'export async function go(kind: string, flag: boolean, deps: { invoke: typeof invoke }) {',
                "    const cmd = flag ? 'fake_then' : 'fake_else';",
                '    await invoke(cmd);',
                '    const entry = EXTRACT[kind];',
                '    await invoke(entry);',
                "    await run('fake_forwarded', 1);",
                "    await invoke(flag ? 'fake_inline_a' : 'fake_inline_b');",
                "    await deps.invoke('fake_injected');",
                '    await invoke(verbFor(kind));',
                "    await runExtract('fake_extract');",
                '}',
                "render(<TagsDialog command=\"fake_drop_tags\" />);",
                "go('zip', true, { invoke });",
            ].join('\n'),
        );
        const files = [extract, dialog, routes];
        const expected = [
            'fake_zip', 'fake_tar', 'fake_then', 'fake_else', 'fake_forwarded', 'fake_inline_a', 'fake_inline_b',
            'fake_injected', 'fake_case_a', 'fake_case_b', 'fake_extract', 'fake_box_tags', 'fake_drop_tags',
        ];
        expect(expected.filter((c) => callersOf(c, files).length === 0)).toEqual([]);
    });

    it('does not count a parameter or a property key named like a wrapper, but counts an import alias', () => {
        const api = scan(
            'src/fake/api.ts',
            [
                "export const createBranch = () => invoke('fake_branch');",
                "export const deleteBranch = () => invoke('fake_delete');",
                'export function apply(deleteBranch: () => void) { deleteBranch(); }',
            ].join('\n'),
        );
        const other = scan(
            'src/fake/other.ts',
            [
                'export const table = { createBranch: 1, deleteBranch: 2 };',
                'export function run(createBranch: () => void) { createBranch(); }',
                'export const read = (o: { deleteBranch: number }) => o.deleteBranch;',
            ].join('\n'),
        );
        expect(callersOf('fake_branch', [api, other])).toEqual([]);
        expect(callersOf('fake_delete', [api, other])).toEqual([]);
        const aliased = scan(
            'src/fake/aliased.ts',
            "import { createBranch as createBranchApi } from './api';\nexport const go = () => createBranchApi();\n",
        );
        expect(callersOf('fake_branch', [api, other, aliased])).toEqual(['src/fake/api.ts via createBranch']);
    });

    it('matches an import to the module it comes from, not to every export of that name', () => {
        // A use of another module's `start` is not a use of this one.
        const starter = scan('src/fake/starter.ts', "export const start = () => invoke('fake_start_cmd');\n");
        const other = scan('src/fake/other.ts', 'export const start = () => 0;\n');
        const user = scan('src/fake/user.ts', "import { start } from './other';\nstart();\n");
        const pkg = scan('src/fake/pkg.ts', "import { start } from 'some-package';\nstart();\n");
        expect(callersOf('fake_start_cmd', [starter, other, user, pkg])).toEqual([]);

        // A forwarder elsewhere does not make a same-named function a forwarder.
        const forwarder = scan('src/fake/forwarder.ts', 'export function run(command: string) { return invoke(command); }\n');
        const logger = scan('src/fake/logger.ts', 'export function run(message: string) { console.log(message); }\n');
        const logs = scan('src/fake/logs.ts', "import { run } from './logger';\nrun('fake_logged_run');\n");
        const forwards = scan('src/fake/forwards.ts', "import { run } from './forwarder';\nrun('fake_forwarded_run');\n");
        const runFiles = [forwarder, logger, logs, forwards];
        expect(callersOf('fake_logged_run', runFiles)).toEqual([]);
        expect(callersOf('fake_forwarded_run', runFiles)).toEqual(['src/fake/forwards.ts']);

        // A constant carrying a command into an invoke does not make a same-named constant carry one.
        const command = scan('src/fake/command.ts', "export const CMD = 'fake_carried_cmd';\n");
        const invoker = scan('src/fake/invoker.ts', "import { CMD } from './command';\ninvoke(CMD);\n");
        const label = scan('src/fake/label.ts', "export const CMD = 'fake_label_cmd';\n");
        const printer = scan('src/fake/printer.ts', "import { CMD } from './label';\nconsole.log(CMD);\n");
        const cmdFiles = [command, invoker, label, printer];
        expect(callersOf('fake_label_cmd', cmdFiles)).toEqual([]);
        expect(callersOf('fake_carried_cmd', cmdFiles)).toEqual(['src/fake/command.ts via CMD']);

        // A prop reaches the component the element was imported from, not every component of that name.
        const action = scan('src/fake/Action.tsx', 'export const Button = ({ name }: { name: string }) => <b onClick={() => invoke(name)} />;\n');
        const plain = scan('src/fake/Plain.tsx', 'export const Button = ({ name }: { name: string }) => <span>{name}</span>;\n');
        const page = scan('src/fake/page.tsx', "import { Button } from './Plain';\nrender(<Button name=\"fake_plain_prop\" />);\n");
        const actions = scan('src/fake/actions.tsx', "import { Button } from './Action';\nrender(<Button name=\"fake_action_prop\" />);\n");
        const propFiles = [action, plain, page, actions];
        expect(callersOf('fake_plain_prop', propFiles)).toEqual([]);
        expect(callersOf('fake_action_prop', propFiles)).toEqual(['src/fake/actions.tsx']);
    });

    it('follows an import through a directory index and through re-exports', () => {
        const impl = scan(
            'src/fake/lib/impl.ts',
            "export const start = () => invoke('fake_barrel_start');\nexport const stop = () => invoke('fake_star_stop');\n",
        );
        const index = scan('src/fake/lib/index.ts', "export { start as begin } from './impl';\n");
        const all = scan('src/fake/all.ts', "export * from './lib/impl';\n");
        const app = scan('src/fake/app.ts', "import { begin } from './lib';\nimport { stop } from './all';\nbegin();\nstop();\n");
        const files = [impl, index, all, app];
        expect(callersOf('fake_barrel_start', files)).toEqual(['src/fake/lib/impl.ts via start']);
        expect(callersOf('fake_star_stop', files)).toEqual(['src/fake/lib/impl.ts via stop']);
    });

    it('resolves every referenced relative import between app modules', () => {
        const byFile = new Set(appFiles.map((f) => f.file));
        const unresolved = appFiles.flatMap((scanned) =>
            scanned.importUses
                .filter(({ ref }) => ref.source !== null && ref.source.startsWith('.') && !isAsset(ref.source))
                .filter(({ ref }) => resolveModule(scanned.file, ref.source as string, byFile) === null)
                .map(({ ref }) => `${scanned.file}: ${ref.source}`),
        );
        expect(unresolved, 'an import the guard cannot follow matches nothing: teach resolveModule its form').toEqual([]);
    });

    it('excludes spec files from the callers', () => {
        expect(isAppSource('src/utils/thing.spec.ts')).toBe(false);
        expect(isAppSource('src/utils/thing.spec.tsx')).toBe(false);
        expect(isAppSource('src/utils/thing.ts')).toBe(true);
    });

    it('follows a wrapper to its callers instead of counting the wrapper itself', () => {
        // `listUsers` wraps `user_partitions_list_users` and is imported across the app.
        expect(callersOf('user_partitions_list_users').some((c) => c.endsWith('via listUsers'))).toBe(true);
        // `createBranch` is imported as `createBranchApi` by useAIChatConversations.ts: the alias is the use.
        expect(callersOf('chat_history_create_branch').some((c) => c.endsWith('via createBranch'))).toBe(true);
    });

    it('registers no new command that nothing in src/ invokes', () => {
        const known = new Set([...INHERITED_UNCALLED, ...Object.keys(AUDITED_UNCALLED)]);
        expect(uncalled.filter((c) => !known.has(c))).toEqual([]);
    });

    it('keeps the audited list exact: every entry is registered and still uncalled', () => {
        const stale = Object.keys(AUDITED_UNCALLED).filter((c) => !uncalled.includes(c));
        expect(stale, 'now called or no longer registered: drop it from AUDITED_UNCALLED').toEqual([]);
        expect(Object.keys(AUDITED_UNCALLED).filter((c) => INHERITED_UNCALLED.includes(c))).toEqual([]);
    });

    it('keeps the inherited list exact: it only shrinks', () => {
        const stale = INHERITED_UNCALLED.filter((c) => !uncalled.includes(c));
        expect(stale, 'now called or no longer registered: drop it from INHERITED_UNCALLED').toEqual([]);
    });
});
