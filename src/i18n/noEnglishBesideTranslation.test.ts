// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { parseAst } from 'vite';

/**
 * A translated string is not glued to English written in the code. The vault
 * toolbar built its tooltips as `t('vault.scrubErrorCorrection') + ' (verify
 * cipher hashes)'`, so every locale showed a translated label followed by an
 * English sentence, and the overlay button prefixed its translated tooltip with
 * `AeroVault ON: `. No locale gate can see this: the English is in the source,
 * not in a locale file.
 *
 * This test finds a `t()` call joined with `+` to a string literal, or placed
 * in a template literal, where the literal text holds a word. A variable used
 * there is followed to its declaration in scope (block, function, loop and catch
 * scopes, with parameters shadowing outer names): the
 * vault repair dialog put `const prefix = dry ? 'Dry-run: ' : ''` in front of
 * its translated result, and a portable device named itself `|| 'Portable
 * device'` in a translated error. Shortcut key names (`(Ctrl+S)`), Aero product names,
 * rclone crypt and a version prefix are not prose and pass. The text belongs in
 * the translation, with the variable part as a `{placeholder}`.
 *
 * JSX renders English written in the code in more shapes, and they are found
 * here too. A JSX child expression that renders a string literal: `{dryRun ?
 * 'Would delete' : 'Deleted'}`, `{busy && 'Loading...'}`, `{label ||
 * 'Copy'}`, `{`${n} server profiles found`}`. JSX text, beside a translated
 * sibling (`{t('cloud.cleanupDone')}: {n} files, {size} freed`) or alone
 * (`<span>Mode:</span>`, the plural tail of `{n} file{n === 1 ? '' : 's'}`).
 * A name rendered as a child that stands for English: a variable declared in
 * scope (`const title = dry ? 'Patch Dry Run Review' : 'Patch Apply Review'`,
 * then `{title}`) or a function of the module called there, whose return
 * values are literals (`{actionLabel(file.action)}`). And the attributes a
 * reader sees or a screen reader speaks, `title`, `placeholder`, `aria-label`,
 * `alt` and `label`, in the same shapes: `title={isPrivate ? 'Private
 * repository' : 'Public repository'}`. Every locale rendered those words in
 * English.
 *
 * In JSX a `t('x') || 'Fallback'` passes: the fallback is dead code
 * (noPhantomKeys.test.ts), never shown. So do CSS in a `<style>` element, the
 * content of `<code>`, `<kbd>`, `<pre>` and `<samp>`, and the tokens in
 * JSX_TOKENS: units, file extensions, HTML entities, URLs, paths, host and file
 * names, identifiers and command-line flags. Short names that read as words
 * but are not prose (product and provider names, S3 storage classes, pricing
 * tiers, sample credentials in a placeholder) are allowed by file and exact
 * text in JSX_NEUTRAL, and an entry that no longer matches a hit fails, so the
 * list cannot outlive its reason.
 *
 * Not covered: a property of an object (`state.label` where `checkState()`
 * returns `{ label: 'passed' }`), a value kept in React state or props, a
 * string from another module, and names inside a function's return value,
 * which belong to the function's scope and not the caller's.
 */

const SOURCES = import.meta.glob<string>(
    ['/src/**/*.{ts,tsx}', '!/src/**/*.test.{ts,tsx}', '!/src/**/*.d.ts'],
    { query: '?raw', import: 'default', eager: true },
);

type Node = { type: string; start: number; end: number; [key: string]: unknown };
const isNode = (value: unknown): value is Node => !!value && typeof value === 'object' && typeof (value as Node).type === 'string';

function children(node: Node): Node[] {
    const out: Node[] = [];
    for (const value of Object.values(node)) {
        if (Array.isArray(value)) out.push(...value.filter(isNode));
        else if (isNode(value)) out.push(value);
    }
    return out;
}

const NEUTRAL = /\b(?:Ctrl|Shift|Alt|Cmd|Meta|Esc|Enter|Tab|Aero[A-Z][A-Za-z]*|AERO[A-Z]+|(?:rclone|RCLONE)(?:[ -](?:crypt|CRYPT))?)\b/g;
const PROSE = /[A-Za-z]{2,}/;

const isT = (node: Node) =>
    node.type === 'CallExpression' && (node.callee as Node).type === 'Identifier' && (node.callee as { name: string }).name === 't';

const containsT = (node: Node): boolean => isT(node) || children(node).some(containsT);

function literalText(node: Node): string | null {
    if (node.type === 'Literal' && typeof node.value === 'string') return node.value;
    if (node.type === 'TemplateLiteral' && (node.expressions as Node[]).length === 0) {
        return (node.quasis as Array<{ value: { cooked: string } }>)[0].value.cooked;
    }
    return null;
}

const isProse = (text: string) => PROSE.test(text.replace(NEUTRAL, ''));

// A value that is never falsy because a translation always comes out of it:
// t() returns a non-empty string. `(ready && t('x')) || 'Ready'` is not one, and
// shows its English fallback whenever ready is false. `??` returns an empty
// string on its left as it is, so only its left side counts.
function neverFalsy(node: Node): boolean {
    if (isT(node)) return true;
    if (node.type === 'ConditionalExpression') return neverFalsy(node.consequent as Node) && neverFalsy(node.alternate as Node);
    if (node.type === 'LogicalExpression') {
        if (node.operator === '||') return neverFalsy(node.left as Node) || neverFalsy(node.right as Node);
        if (node.operator === '&&') return neverFalsy(node.left as Node) && neverFalsy(node.right as Node);
        return neverFalsy(node.left as Node);
    }
    if (['ParenthesizedExpression', 'TSAsExpression', 'TSNonNullExpression'].includes(node.type)) return neverFalsy(node.expression as Node);
    return false;
}

// The texts a declaration can evaluate to: a literal, either branch of a
// condition, the value side of `&&`, or either side of `||` / `??`. A fallback
// after a value that is never falsy is not one, since it never shows.
function texts(node: Node): string[] {
    const text = literalText(node);
    if (text !== null) return [text];
    if (node.type === 'ConditionalExpression') return [...texts(node.consequent as Node), ...texts(node.alternate as Node)];
    if (node.type === 'LogicalExpression') {
        if (node.operator === '&&') return texts(node.right as Node);
        if (!neverFalsy(node.left as Node)) return [...texts(node.left as Node), ...texts(node.right as Node)];
    }
    return [];
}

// Nodes that open a lexical scope for `const`, `let`, parameters and catch bindings.
const SCOPES = new Set([
    'BlockStatement', 'StaticBlock', 'ForStatement', 'ForInStatement', 'ForOfStatement', 'CatchClause',
    'FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression',
]);

// The names a binding pattern introduces: `a`, `{ a, b: c }`, `[a, ...d]`, `a = 1`, `a: string`.
function bindingNames(pattern: Node): string[] {
    if (pattern.type === 'Identifier') return [(pattern as unknown as { name: string }).name];
    if (pattern.type === 'AssignmentPattern') return bindingNames(pattern.left as Node);
    if (pattern.type === 'RestElement') return bindingNames(pattern.argument as Node);
    if (pattern.type === 'TSParameterProperty') return bindingNames(pattern.parameter as Node);
    if (pattern.type === 'ArrayPattern') return (pattern.elements as Array<Node | null>).filter(isNode).flatMap(bindingNames);
    if (pattern.type === 'ObjectPattern') {
        return (pattern.properties as Node[]).flatMap((property) =>
            bindingNames((property.type === 'Property' ? property.value : property) as Node));
    }
    return [];
}

/**
 * Neutral where JSX renders them: HTML entities, URLs, e-mail addresses, host
 * and file names with an optional path (`gitlab.com/owner/repo`), paths,
 * identifiers with an underscore, command-line flags, file extensions and units.
 */
const JSX_TOKENS = new RegExp([
    /&#?\w+;/,
    /\b[a-z][a-z0-9+.-]*:\/\/\S*/,
    /[\w.+-]+@[\w-]+(?:\.[\w-]+)+/,
    /\b[\w-]+(?:\.[\w-]+)*\.[A-Za-z]{2,}\b(?:\/[\w./*{}-]*)?/,
    /(?<![\w/])~?\/[\w./*{}-]*/,
    /\b\w*_\w*\b/,
    /(?<![\w-])--[a-z][\w-]*/,
    /(?<![\w.])(?:\.[a-z][a-z0-9-]*)+\b/,
    /(?<![A-Za-z])(?:B|KB|MB|GB|TB|KiB|MiB|GiB|TiB|Mbps|ms|min|px)(?![A-Za-z])/,
].map((pattern) => pattern.source).join('|'), 'g');

const isJsxProse = (text: string) => isProse(text.replace(JSX_TOKENS, ''));

/** Text JSX renders as it is, by file. Each entry must still match a hit. */
const JSX_NEUTRAL: Record<string, string[]> = {
    // S3 storage-class badges, the update's package format, the PRO tier badge and GitHub product names.
    'src/App.tsx': [
        'IA', '1Z-IA', 'Glacier', 'Glacier IR', 'Deep', 'IT', 'RR', 'deb', 'PRO', 'GitHub Pages', 'GitHub Actions',
    ],
    // The protocol id shown when a profile has none.
    'src/components/BridgeSourcePanel.tsx': ['ftp'],
    'src/components/ExportImportDialog.tsx': ['ftp'],
    'src/components/MountManagerDialog.tsx': ['ftp'],
    'src/components/ServerHealthCheck.tsx': ['ftp'],
    'src/components/SpeedTestDialog.tsx': ['ftp'],
    'src/components/common/ProfileRail.tsx': ['ftp'],
    // Log source and test runner tags.
    'src/components/DebugPanel.tsx': ['RUST', 'JS', 'rust', 'js'],
    // GitLab merge request, GitHub pull request.
    'src/components/GitHubWriteModeIndicator.tsx': ['MR', 'PR'],
    // The FTP SITE command and its TLS marker.
    'src/components/SiteCommand/SiteCommandDialog.tsx': ['· TLS', 'SITE'],
    // The player badge: its cyber theme and the OpenGL visualizer mode.
    'src/components/Preview/viewers/AudioPlayer.tsx': ['// CYBER', 'GL'],
    'src/components/DevTools/GeminiCodeBlock.tsx': ['OK'],
    // End of file, where a patch hunk expected more lines.
    'src/components/DevTools/CodingPatchReview.tsx': ['EOF'],
    // Pricing tier badges.
    'src/components/AISettings/ProviderMarketplace.tsx': ['FREE', 'FREEMIUM', 'PAID'],
    // The trash dialog title names its provider.
    'src/components/BoxTrashManager.tsx': ['- Box'],
    'src/components/DropboxTrashManager.tsx': ['- Dropbox'],
    'src/components/GoogleDriveTrashManager.tsx': ['- Google Drive'],
    'src/components/JottacloudTrashManager.tsx': ['- Jottacloud'],
    'src/components/KoofrTrashManager.tsx': ['- Koofr'],
    'src/components/MegaTrashManager.tsx': ['- MEGA'],
    'src/components/OneDriveTrashManager.tsx': ['- OneDrive'],
    'src/components/ZohoTrashManager.tsx': ['- Zoho WorkDrive'],
    // The font preview sample, the providers of the OAuth app list and a client id sample.
    'src/components/SettingsPanel.tsx': [
        '- AaBbCcDd 123', 'Google API', 'Google Drive, Google Photos', 'Dropbox', 'OneDrive', 'Box', 'pCloud Drive',
        '4shared', 'Zoho WorkDrive', 'Yandex Disk', 'GitHub', 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx',
    ],
    // The fixed API key placeholder of Ollama, which needs none.
    'src/components/AISettings/AISettingsPanel.tsx': ['ollama'],
    // Protocol and provider lists, the license, a team member, the toolchain and a support site.
    'src/components/AboutDialog.tsx': [
        'FTP / FTPS / SFTP / WebDAV / S3 / GitHub', 'Google Drive / Dropbox / OneDrive / MEGA / Box / Filen',
        'GNU General Public License v3.0', 'E.K., PhD', 'Tauri', 'Rust', 'SourceForge',
    ],
    // The word the code compares the input with, named by the translated label.
    'src/components/AeroCryptKeyslotsModal.tsx': ['YES'],
    // Sample exclude patterns.
    'src/components/AeroSync/PlanTabContent.tsx': ['cache, *.tmp, build/output'],
    // Sample credentials and identifiers, a provider name and the AeroShare id label.
    'src/components/ConnectionScreen.tsx': [
        'aeroftp', 'demopublickey...', 'dxz9abc12', '003d90ca9d33900000000001', 'my-b2-bucket', 'glpat-xxxxxxxxxxxx',
        'FEF5078EA41D182EEF89A21E034BD680', 'AKIAIOSFODNN7EXAMPLE', 'aeroftp2026', 'FileLu', 'AeroFTP-ID',
    ],
    // Cipher names.
    'src/components/CryptoLabTab.tsx': ['AES-256-GCM', 'ChaCha20-Poly1305'],
    // Key derivation and key wrap algorithm names.
    'src/components/CryptomatorBrowser.tsx': ['scrypt KDF', 'AES Key Wrap'],
    // A sample vault name.
    'src/components/CryptomatorCreateDialog.tsx': ['MyVault'],
    // The hash algorithm before a digest.
    'src/components/DevTools/CodingCheckpointRestoreReview.tsx': ['sha256'],
    // Output stream names.
    'src/components/DevTools/CodingChecksReview.tsx': ['stdout', 'stderr'],
    // The protocol name.
    'src/components/DevTools/SSHTerminal.tsx': ['SSH'],
    // Hash algorithm names.
    'src/components/Dialogs/index.tsx': ['MD5', 'SHA-1', 'SHA-256', 'SHA-512', 'BLAKE3'],
    // Product names.
    'src/components/GitHubActionsBrowser.tsx': ['GitHub Actions'],
    // Product names, a protocol and a DNS record type.
    'src/components/GitHubPagesBrowser.tsx': ['GitHub Pages', 'HTTPS', 'CNAME'],
    // A sample hex key.
    'src/components/HashForgeTab.tsx': ['0123...cdef'],
    // Image format names.
    'src/components/IconPickerDialog.tsx': ['SVG, PNG, JPG, WEBP, GIF, ICO'],
    // Protocol ids, cipher names and the Nextcloud OCS API badge.
    'src/components/IntroHub/ServerCard.tsx': ['MTP', 'ftps', 'ftp', 'XSalsa20-Poly1305', 'AES-256-GCM-SIV', 'API OCS'],
    // A protocol and a store name.
    'src/components/McpDialog.tsx': ['Model Context Protocol', 'VS Code Marketplace'],
    // The provider name.
    'src/components/NextcloudTrashManager.tsx': ['Nextcloud'],
    // The equalizer badge, in the style of the player badge.
    'src/components/Preview/controls/AudioMixer.tsx': ['// EQ'],
    // The OpenGL visualizer mode.
    'src/components/Preview/controls/WebGLVisualizer.tsx': ['GL'],
    // Provider names as logo alt text.
    'src/components/ProviderLogos.tsx': [
        'Koofr', 'OpenDrive', 'Filebase', 'Yandex Disk', 'Proton Drive', 'Jianguoyun', 'PixelUnion', 'Twake Drive',
        'DriveHQ', 'FileLu', 'Felicloud', 'S3Drive', 'Blomp', 'AWS', 'MinIO', 'Quotaless', 'Hetzner', 'ASPnix',
        'ImageKit', 'Uploadcare', 'Cloudinary', 'IBM Cloud', 'Mail.ru Cloud',
    ],
    // The PRO tier badge and the S3/Azure badge.
    'src/components/ProvidersDialog.tsx': ['PRO', 'S3/AZ'],
    // The protocol id shown when a session has no provider.
    'src/components/SessionTabs.tsx': ['FTP'],
    // The otpauth URI scheme.
    'src/components/TotpSetup.tsx': ['otpauth URI:'],
    // The verifier library.
    'src/components/UpdateVerificationPanel.tsx': ['sigstore-rs'],
    // The Sigstore logo.
    'src/components/icons/SigstoreMark.tsx': ['Sigstore'],
    // The file extension and the cipher.
    'src/components/vault/VaultCreate.tsx': ['.aerovault · AES-256'],
};

const squash = (text: string) => text.replace(/\s+/g, ' ').trim();

/**
 * The string nodes a JSX child expression renders: the same cases as texts(),
 * plus a template literal with no `t()` in it, both its text and what its
 * interpolations render (one with `t()` is the template check above), the
 * operands of a `+` with no `t()` in it (one with `t()` is the concatenation
 * check above) and TypeScript wrappers.
 */
function rendered(node: Node, out: Node[], resolve: (node: Node) => Node[] = () => []): void {
    const again = (next: Node) => rendered(next, out, resolve);
    if (literalText(node) !== null) out.push(node);
    else if (node.type === 'TemplateLiteral' && !(node.expressions as Node[]).some(containsT)) {
        out.push(node);
        (node.expressions as Node[]).forEach(again);
    }
    else if (node.type === 'ConditionalExpression') {
        again(node.consequent as Node);
        again(node.alternate as Node);
    } else if (node.type === 'LogicalExpression') {
        if (node.operator !== '&&' && neverFalsy(node.left as Node)) return;
        if (node.operator !== '&&') again(node.left as Node);
        again(node.right as Node);
    } else if (node.type === 'BinaryExpression' && node.operator === '+' && !containsT(node)) {
        again(node.left as Node);
        again(node.right as Node);
    } else if (['ParenthesizedExpression', 'TSAsExpression', 'TSNonNullExpression'].includes(node.type)) {
        again(node.expression as Node);
    } else if (node.type === 'Identifier') {
        resolve(node).forEach(again);
    } else if (node.type === 'CallExpression') {
        // A return value is read in the function's own scope, which is not the
        // caller's: its names are not followed.
        resolve(node).forEach((value) => rendered(value, out));
    }
}

const renderedText = (node: Node) =>
    literalText(node) ?? (node.quasis as Array<{ value: { cooked: string } }>).map((quasi) => quasi.value.cooked).join(' ');

const elementName = (node: Node) =>
    node.type === 'JSXElement' ? ((node.openingElement as Node).name as { name?: string }).name : undefined;

const isStyleElement = (node: Node) => elementName(node) === 'style';

/** Elements whose text is code: a command, a key, a sample. */
const CODE_ELEMENTS = new Set(['code', 'kbd', 'pre', 'samp']);

/** Attributes a reader sees or a screen reader speaks. */
const PROSE_ATTRIBUTES = new Set(['title', 'placeholder', 'aria-label', 'alt', 'label']);

const attributeName = (node: Node) => (node.name as { name?: unknown }).name;

// The expressions a function can return: an arrow's expression body, or the
// argument of each return statement outside nested functions.
function returned(fn: Node): Node[] {
    const body = fn.body as Node;
    if (body.type !== 'BlockStatement') return [body];
    const out: Node[] = [];
    const walk = (node: Node) => {
        if (node.type === 'ReturnStatement' && isNode(node.argument)) out.push(node.argument);
        if (!SCOPES.has(node.type) || node === body) children(node).forEach(walk);
    };
    walk(body);
    return out;
}

const isFunction = (node: Node | null | undefined): node is Node =>
    !!node && ['FunctionDeclaration', 'FunctionExpression', 'ArrowFunctionExpression'].includes(node.type);

function scan(sources: Record<string, string>, allowed: Record<string, string[]> = {}): { found: string[]; unused: string[] } {
    const found: string[] = [];
    const unused = new Set(Object.entries(allowed).flatMap(([file, texts]) => texts.map((text) => `${file} ${text}`)));
    for (const [file, source] of Object.entries(sources)) {
        const name = file.replace(/^\//, '');
        const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
        const report = (node: Node, start = node.start) => {
            const line = source.slice(0, start).split('\n').length;
            const entry = `${name}:${line} ${squash(source.slice(start, node.end)).slice(0, 90)}`;
            if (!found.includes(entry)) found.push(entry);
        };
        const neutral = (text: string) => {
            if (!allowed[name]?.includes(squash(text))) return !isJsxProse(text);
            unused.delete(`${name} ${squash(text)}`);
            return true;
        };
        // One map per lexical scope, innermost last. A name resolves to the closest
        // scope that declares it; a parameter shadows with no value (null).
        const scopes: Array<Map<string, Node | null>> = [new Map()];
        // A declaration is visible to the whole scope that holds it: a component
        // calls a helper the module declares further down.
        const hoist = (scope: Map<string, Node | null>, statements: Node[]) => {
            for (const statement of statements) {
                const declaration = statement.type.startsWith('Export') ? statement.declaration : statement;
                if (!isNode(declaration)) continue;
                if (declaration.type === 'FunctionDeclaration' && isNode(declaration.id)) {
                    scope.set((declaration.id as unknown as { name: string }).name, declaration);
                }
                if (declaration.type !== 'VariableDeclaration') continue;
                for (const declarator of declaration.declarations as Node[]) {
                    if ((declarator.id as Node).type !== 'Identifier') continue;
                    scope.set((declarator.id as unknown as { name: string }).name, isNode(declarator.init) ? declarator.init : null);
                }
            }
        };
        hoist(scopes[0], program.body as Node[]);
        // What a rendered name stands for: the value a variable was declared with,
        // or what a function returns when it is called. Each check starts afresh,
        // so a name that refers to itself is followed once.
        const resolver = () => {
            const seen = new Set<Node>();
            return (node: Node): Node[] => {
                const call = node.type === 'CallExpression';
                const target = call ? node.callee as Node : node;
                if (target.type !== 'Identifier') return [];
                const id = (target as unknown as { name: string }).name;
                const value = [...scopes].reverse().find((candidate) => candidate.has(id))?.get(id);
                if (!value || seen.has(value) || call !== isFunction(value)) return [];
                seen.add(value);
                return call ? returned(value) : [value];
            };
        };
        const proseVariable = (node: Node) => {
            if (node.type !== 'Identifier') return false;
            const name = (node as unknown as { name: string }).name;
            const scope = [...scopes].reverse().find((candidate) => candidate.has(name));
            const init = scope?.get(name);
            return !!init && texts(init).some(isProse);
        };
        const visit = (node: Node) => {
            if (isStyleElement(node)) return;
            const opensScope = SCOPES.has(node.type);
            if (opensScope) {
                const scope = new Map<string, Node | null>();
                for (const param of (node.params as Node[] | undefined) ?? []) bindingNames(param).forEach((name) => scope.set(name, null));
                if (node.type === 'CatchClause' && isNode(node.param)) bindingNames(node.param).forEach((name) => scope.set(name, null));
                scopes.push(scope);
            }
            if (node.type === 'BlockStatement' || node.type === 'StaticBlock') hoist(scopes[scopes.length - 1], node.body as Node[]);
            if (node.type === 'VariableDeclarator') {
                const scope = scopes[scopes.length - 1];
                if ((node.id as Node).type === 'Identifier') {
                    scope.set((node.id as unknown as { name: string }).name, isNode(node.init) ? node.init : null);
                } else {
                    bindingNames(node.id as Node).forEach((name) => scope.set(name, null));
                }
            }
            if (node.type === 'BinaryExpression' && node.operator === '+') {
                const left = node.left as Node;
                const right = node.right as Node;
                const other = containsT(left) ? right : containsT(right) ? left : null;
                const text = other ? literalText(other) : null;
                if ((text !== null && isProse(text)) || (other && proseVariable(other))) report(node);
            }
            if (node.type === 'TemplateLiteral' && (node.expressions as Node[]).some(containsT)) {
                const quasis = node.quasis as Array<{ value: { cooked: string } }>;
                const expressions = node.expressions as Node[];
                if (quasis.some((quasi) => isProse(quasi.value.cooked)) || expressions.some(proseVariable)) {
                    report(node);
                }
            }
            // Every neutral() call runs, so each allowance that matches is marked used.
            const english = (expression: Node) => {
                const strings: Node[] = [];
                rendered(expression, strings, resolver());
                return strings.map((string) => neutral(renderedText(string))).includes(false);
            };
            if ((node.type === 'JSXElement' || node.type === 'JSXFragment') && !CODE_ELEMENTS.has(elementName(node) ?? '')) {
                for (const kid of node.children as Node[]) {
                    if (kid.type === 'JSXExpressionContainer' && english(kid.expression as Node)) report(kid);
                    if (kid.type === 'JSXText' && !neutral(kid.value as string)) {
                        report(kid, kid.start + (kid.value as string).search(/\S/));
                    }
                }
            }
            if (node.type === 'JSXAttribute' && PROSE_ATTRIBUTES.has(attributeName(node) as string) && isNode(node.value)) {
                const value = node.value as Node;
                if (english(value.type === 'JSXExpressionContainer' ? value.expression as Node : value)) report(node);
            }
            children(node).forEach(visit);
            if (opensScope) scopes.pop();
        };
        visit(program);
    }
    return { found, unused: [...unused] };
}

const offenders = (sources: Record<string, string>) => scan(sources).found;

describe('a translated string is not glued to English in the code', () => {
    it('finds the shapes it is meant to find, and passes the neutral ones', () => {
        const sample = [
            "const a = t('vault.scrubErrorCorrection') + ' (verify cipher hashes)';",
            "const b = t('vault.remote.open') + ': .aerovault';",
            "const c = `AeroVault ON: ${t('toolbar.aerovaultOverlayActive') || 'fallback'}`;",
            "const d = 'Error: ' + t('common.failed');",
            "const e = `${t('devtools.editorPanel.save')} (Ctrl+S)`;",
            "const f = `${t('statusBar.updateAvailable')}: AeroFTP v${version}`;",
            "const g = t('vault.repairErrorCorrectionTooltip', { label: t('vault.repairErrorCorrection') });",
            "const h = t('a') + ' ' + t('b');",
            "const i = `${t('guiController.banner')}: ${ok ? t('common.open') : t('common.close')}`;",
            "const prefix = dry ? 'Dry-run: ' : '';",
            "const j = `${prefix}${t('vault.noDamageNothing')}`;",
            "const label = isAero ? 'AEROCRYPT' : 'RCLONE CRYPT';",
            "const k = `${label} · ${t('aerocrypt.lock')}`;",
            "const tail = dry && ' (no changes written)';",
            "const m = t('vault.close') + tail;",
            "const name = profile?.name || device.displayName || 'Portable device';",
            "const n = `${t('sidebar.portable_open_failed')}: ${name}`;",
            "const shown = t('vault.close') || 'Close';",
            "const o = t('vault.open') + shown;",
            "const prefix2 = 'Error: ';",
            "function helper() { const prefix2 = ''; return prefix2; }",
            "const p = `${prefix2}${t('vault.close')}`;",
            "const quiet = '';",
            "function loud() { const quiet = 'Loud text'; return quiet; }",
            "const q = `${quiet}${t('vault.close')}`;",
            "const label2 = 'Some text';",
            "const fmt = (label2: string) => `${label2}${t('vault.close')}`;",
            "const state = (ready && t('vault.open')) || 'Closed';",
            "const r = `${state}${t('vault.close')}`;",
        ].join('\n');
        expect(offenders({ 'sample.ts': sample })).toEqual([
            "sample.ts:1 t('vault.scrubErrorCorrection') + ' (verify cipher hashes)'",
            "sample.ts:2 t('vault.remote.open') + ': .aerovault'",
            "sample.ts:3 `AeroVault ON: ${t('toolbar.aerovaultOverlayActive') || 'fallback'}`",
            "sample.ts:4 'Error: ' + t('common.failed')",
            "sample.ts:11 `${prefix}${t('vault.noDamageNothing')}`",
            "sample.ts:15 t('vault.close') + tail",
            "sample.ts:17 `${t('sidebar.portable_open_failed')}: ${name}`",
            "sample.ts:22 `${prefix2}${t('vault.close')}`",
            "sample.ts:29 `${state}${t('vault.close')}`",
        ]);
    });

    it('finds the JSX shapes, and passes the neutral ones', () => {
        const sample = [
            "const a = <span>{dryRun ? 'Would delete' : 'Deleted'}</span>;",
            "const b = <span>{busy && 'Loading...'}</span>;",
            "const c = <span>{label || 'Copy'}</span>;",
            "const d = <span>{`${n} server profiles found`}</span>;",
            "const e = <p>{t('cloud.cleanupDone')}: {n} files, {size} freed</p>;",
            "const f = <span>{t('common.copy') || 'Copy'}</span>;",
            "const g = <span>{dryRun ? t('a.dryRun') : t('a.apply')}</span>;",
            "const h = <span>{isZip ? '.aerozip' : '.aerovault'} {`${size} MB`} {ms < 1 ? '<1ms' : `${ms}ms`}</span>;",
            "const i = <h2>{t('contextMenu.trashTitle')} - Box &middot; (MB)</h2>;",
            "const j = <style>{`@keyframes blink { from { opacity: 0 } }`}</style>;",
            "const k = <span>{tier === 'free' ? 'FREE' : 'PAID'}</span>;",
            "const l = <p>{t('a.b')} <strong>{name}</strong></p>;",
            "const m = <span>{count + ' files'}</span>;",
            "const n = <span>{(ready && t('a.ready')) || 'Ready'}</span>;",
            "const o = <p><strong>{t('a.done')}</strong> files</p>;",
            "const p = <span>{(ok ? t('a.x') : t('a.y')) || 'Fallback'} {size + 'px'}</span>;",
            "const q = <span>{`${'Delete'} ${count}`} {`${ok ? 'Done' : ''} ${n} MB`}</span>;",
        ].join('\n');
        const allowed = { 'sample.tsx': ['FREE', 'PAID', '- Box &middot; (MB)', 'never seen'] };
        expect(scan({ 'sample.tsx': sample }, allowed)).toEqual({
            found: [
                "sample.tsx:1 {dryRun ? 'Would delete' : 'Deleted'}",
                "sample.tsx:2 {busy && 'Loading...'}",
                "sample.tsx:3 {label || 'Copy'}",
                'sample.tsx:4 {`${n} server profiles found`}',
                'sample.tsx:5 files,',
                'sample.tsx:5 freed',
                "sample.tsx:13 {count + ' files'}",
                "sample.tsx:14 {(ready && t('a.ready')) || 'Ready'}",
                'sample.tsx:15 files',
                "sample.tsx:17 {`${'Delete'} ${count}`}",
                "sample.tsx:17 {`${ok ? 'Done' : ''} ${n} MB`}",
            ],
            unused: ['sample.tsx never seen'],
        });
    });

    it('finds JSX text, rendered names and prose attributes, and passes the neutral ones', () => {
        const sample = [
            "const actionLabel = (dry: boolean) => { if (dry) return 'would delete'; return 'deleted'; };",
            "function Card({ dry, n, label }: { dry: boolean; n: number; label: string }) {",
            "    const title = dry ? 'Patch Dry Run Review' : 'Patch Apply Review';",
            "    const code = 'stdout';",
            "    return (",
            "        <div title={dry ? 'Private repository' : 'Public repository'}>",
            "            <span>Mode:</span>",
            "            <span>{n} file{n === 1 ? '' : 's'}</span>",
            "            <span>{title}</span>",
            "            <span>{actionLabel(dry)}</span>",
            "            <span>{code} {label}</span>",
            `            <input placeholder="Pair name" aria-label={t('a.close') || 'Close'} />`,
            '            <img alt="Koofr" />',
            "            <code>ssh-keygen -t ed25519</code>",
            "            <span>https://docs.aeroftp.app/ /path/to/source --dry-run .aerovault 12 KiB</span>",
            '            <input placeholder="email@example.com" title={`${label}`} />',
            "        </div>",
            "    );",
            "}",
            "function Later() { return <span>{helper()}</span>; }",
            "function helper() { return 'Declared further down'; }",
            "function Shadow({ title }: { title: string }) { return <span>{title}</span>; }",
        ].join('\n');
        const allowed = { 'sample.tsx': ['Koofr', 'stdout'] };
        expect(scan({ 'sample.tsx': sample }, allowed)).toEqual({
            found: [
                "sample.tsx:6 title={dry ? 'Private repository' : 'Public repository'}",
                'sample.tsx:7 Mode:',
                'sample.tsx:8 file',
                'sample.tsx:9 {title}',
                'sample.tsx:10 {actionLabel(dry)}',
                'sample.tsx:12 placeholder="Pair name"',
                'sample.tsx:20 {helper()}',
            ],
            unused: [],
        });
    });

    // Parsing every source file takes about 3 s on an idle machine and over 10 s
    // under a load average of 30, past vitest's default 5 s.
    it('no source file of the app does', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(scan(SOURCES, JSX_NEUTRAL)).toEqual({ found: [], unused: [] });
    }, 30_000);
});
