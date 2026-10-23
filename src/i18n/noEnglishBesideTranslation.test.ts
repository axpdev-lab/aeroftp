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
 * A rendered name is followed wherever its value comes from: a property of an
 * object or an array element (`{state.label}` where `checkState()` returns
 * `{ label: 'passed' }`, `{STATUS[run.status].label}`, `.find(...)?.label`),
 * the element a callback receives (`OPTIONS.map((o) => o.label)`,
 * `Object.entries(MAP)`), a state (its initial value and every value passed
 * to its setter: `setError('Invalid characters in vault name')` then
 * `{error}`), a prop's default (`({ title = 'Untitled' })`), and a value or a
 * function imported from another module (`import { FONT_PRESETS } from
 * '../utils/fontPresets'`). Text props of a component (`heading="Saved
 * servers"`, `confirmLabel=`) and properties named for text in a `.tsx`
 * object (`{ label: '1 hour', value: 3600 }`, `{ description: 'Personal cloud
 * sync' }`) are checked the same way. A string reached through a name is
 * reported where it is written, in whichever module. An abbreviation of single
 * letters ("e.g.", "i.e.") is English all the same.
 *
 * In JSX a `t('x') || 'Fallback'` passes: the fallback is dead code
 * (noPhantomKeys.test.ts), never shown. So do CSS in a `<style>` element, the
 * content of `<code>`, `<kbd>`, `<pre>` and `<samp>`, and the tokens in
 * JSX_TOKENS: units, file extensions, HTML entities, URLs, paths, host and file
 * names, identifiers, command-line flags, hex colors and MIME types; a list of
 * CSS classes; and an expression rendered right after a "." in JSX text, a
 * file extension. Some strings are data by their shape and pass without a
 * list: the arguments of the console, of the backend (`invoke`) and of `JSON`;
 * an object with a snake_case key, which mirrors a backend or runner payload
 * (`{ status, duration_ms, message }`); and an identifier-like value of a
 * property that names a key (`{ name: 'path' }`, `{ type: 'string' }`); an
 * identifier-like value the code compares somewhere (`status === 'built'`,
 * `code === 'global'`, a protocol id), unless it is interpolated into a
 * sentence; and a two-letter code (`IT`, `US`). Short
 * names that read as words
 * but are not prose (product and provider names, S3 storage classes, pricing
 * tiers, sample credentials in a placeholder) are allowed by file and exact
 * text in JSX_NEUTRAL, and an entry that no longer matches a hit fails, so the
 * list cannot outlive its reason.
 *
 * A function's return values are read in the function's own scope: its
 * parameters and the declarations of its body (`const verb = pct >= 0 ?
 * 'saved' : 'grew'; return `${verb} ${pct}%``).
 *
 * A value passed to `t()` as an interpolation parameter is part of the
 * translated sentence and is checked the same way (`t('x', { reason: 'Fatal
 * error' })`), also when the parameters are a record declared elsewhere or
 * spread into the call, unless its parameter names an entity (`provider`,
 * `server`, an address, `protocol`, a plan, a shell, a storage class, a
 * command): such a value is a name. A value the code compares is still text
 * there, since it reads as part of the sentence. A placeholder marker a sentence is split on (`{ link: '{link}' }`) is
 * not text either.
 *
 * Not covered: a value passed as a prop and rendered inside another component
 * under a name that is not a text name (`<Select options={OPTIONS} />` when
 * OPTIONS is not followed from a render site), a value that crosses an
 * `await` or a promise, and a string built at run time from data.
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

/** An abbreviation of single letters, such as "e.g." or "i.e.": English all the same. */
const ABBREVIATION = /(?<![\w.])(?:[A-Za-z]\.){2,}/;

const isProse = (text: string) => {
    const words = text.replace(NEUTRAL, '');
    return PROSE.test(words) || ABBREVIATION.test(words);
};

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
    /(?<![A-Za-z])(?:B|KB|MB|GB|TB|PB|EB|KiB|MiB|GiB|TiB|PiB|EiB|Mbps|ms|min|px)(?![A-Za-z])/,
    /#[0-9a-fA-F]{3,8}\b/,
    /\b(?:application|text|image|audio|video|font|multipart)\/[\w.+-]+/,
].map((pattern) => pattern.source).join('|'), 'g');

/** A list of CSS classes, `text-gray-700 dark:text-gray-300`: every token is a utility class. */
const isClassList = (text: string) => {
    const tokens = text.trim().split(/\s+/);
    return tokens[0] !== '' && tokens.every((token) => /^!?-?[a-z0-9][\w\-:/.[\]#%()]*$/.test(token) && /[-:[]/.test(token));
};

/** A two-letter code (`IT`, `US`): a country, language or region, not a word. */
const isCode = (text: string) => /^[A-Z]{2}$/.test(text.trim());

const isJsxProse = (text: string) => !isClassList(text) && !isCode(text) && !isMarker(text) && isProse(text.replace(JSX_TOKENS, ''));

/** Text JSX renders as it is, by file. Each entry must still match a hit. */
const JSX_NEUTRAL: Record<string, string[]> = {
    // S3 storage-class badges, the PRO tier badge and GitHub product names.
    'src/App.tsx': [
        'IA', '1Z-IA', 'Glacier', 'Glacier IR', 'Deep', 'IT', 'RR', 'PRO', 'GitHub Pages', 'GitHub Actions',
        'Zoho Writer', 'Zoho Sheet', 'Zoho Show', '[mega-df]', 'Writer', 'Sheet', 'Show', '(AES-256)',
    ],
    // Log source and test runner tags.
    'src/components/DebugPanel.tsx': ['RUST', 'JS', 'js'],
    // GitLab merge request, GitHub pull request.
    'src/components/GitHubWriteModeIndicator.tsx': ['MR', 'PR'],
    // The FTP SITE command and its TLS marker.
    'src/components/SiteCommand/SiteCommandDialog.tsx': ['· TLS', 'SITE'],
    // The player badge: its cyber theme and the OpenGL visualizer mode.
    'src/components/Preview/viewers/AudioPlayer.tsx': ['// CYBER', 'GL', '// GL:'],
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
    // The Model Context Protocol tab.
    'src/components/AISettings/AISettingsPanel.tsx': ['MCP'],
    // Protocol and provider lists, the license, a team member, the toolchain and a support site.
    'src/components/AboutDialog.tsx': [
        'FTP / FTPS / SFTP / WebDAV / S3 / GitHub', 'Google Drive / Dropbox / OneDrive / MEGA / Box / Filen',
        'GNU General Public License v3.0', 'E.K., PhD', 'Tauri', 'Rust', 'SourceForge', 'React', 'TypeScript',
        'Tailwind CSS', 'Monaco Editor', 'Vite', 'A.P.', 'SSH/SFTP', 'FTP/FTPS', 'HTTP', 'AES-256-GCM', 'KDF',
        '7z AES-256', 'WebDAV', 'OAuth2',
    ],
    // The word the code compares the input with, named by the translated label.
    'src/components/AeroCryptKeyslotsModal.tsx': ['YES'],
    // Sample exclude patterns.
    'src/components/AeroSync/PlanTabContent.tsx': ['cache, *.tmp, build/output'],
    // Sample credentials and identifiers, a provider name, the AeroShare id label and
    // the Proton Drive tab a copy is saved to.
    'src/components/ConnectionScreen.tsx': [
        'demopublickey...', 'dxz9abc12', '003d90ca9d33900000000001', 'my-b2-bucket', 'glpat-xxxxxxxxxxxx',
        'FEF5078EA41D182EEF89A21E034BD680', 'AKIAIOSFODNN7EXAMPLE', 'aeroftp2026', 'FileLu', 'AeroFTP-ID',
        'PixelUnion', 'Immich', 'Dropbox', 'Google Drive', 'OneDrive', 'Box', 'Google Photos', 'pCloud Drive',
        'Filen', 'Internxt', 'Jottacloud', 'Proton Drive', 'kDrive', 'Yandex Disk', 'Zoho WorkDrive', '4shared',
        'Backblaze B2', 'Drime', 'MEGA', 'Cloudinary', 'ImageKit', 'Twake Drive', 'Uploadcare', 'GitHub', 'GitLab',
        'Proton', 'API', 'CLI',
    ],
    // The Proton Drive tab (API or CLI) a mode switch goes to.
    'src/components/ProtonModeSwitchDialog.tsx': ['API', 'CLI'],
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
    'src/components/DevTools/SSHTerminal.tsx': [
        'SSH', 'Tokyo Night', 'Dracula', 'Monokai', 'Solarized Dark', 'Solarized Light', 'GitHub Dark', 'Nord',
        'Catppuccin Mocha',
    ],
    // Hash algorithm names.
    'src/components/Dialogs/index.tsx': [
        'MD5', 'SHA-1', 'SHA-256', 'SHA-512', 'BLAKE3', 'QuickXor', 'Dropbox', 'Koofr', 'Git SHA-1', 'CRC32',
        'Adler-32',
    ],
    // Product names.
    'src/components/GitHubActionsBrowser.tsx': ['GitHub Actions'],
    // Product names, a protocol and a DNS record type.
    'src/components/GitHubPagesBrowser.tsx': ['GitHub Pages', 'HTTPS', 'CNAME'],
    // A sample hex key.
    'src/components/HashForgeTab.tsx': ['0123...cdef', 'MD5', 'SHA-1', 'SHA-256', 'SHA-512', 'BLAKE3', 'Argon2id'],
    // Image format names.
    'src/components/IconPickerDialog.tsx': ['SVG, PNG, JPG, WEBP, GIF, ICO'],
    // Protocol ids, cipher names and the Nextcloud OCS API badge.
    'src/components/IntroHub/ServerCard.tsx': ['MTP', 'XSalsa20-Poly1305', 'AES-256-GCM-SIV', 'API OCS'],
    // A protocol and a store name.
    'src/components/McpDialog.tsx': [
        'Model Context Protocol', 'VS Code Marketplace', 'Claude Code', 'Claude Desktop (Win)',
        'Claude Desktop (Mac)', 'Cursor', 'Windsurf',
    ],
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
    'src/components/ProvidersDialog.tsx': [
        'PRO', 'S3/AZ', 'Google Drive', 'Dropbox', 'OneDrive', 'Box', 'Zoho WorkDrive', 'pCloud Drive', 'MEGA (API)',
        'MEGA (MEGAcmd)', 'Filen', 'Internxt', 'kDrive', 'Jottacloud', 'FileLu', 'Twake Drive', 'Koofr', 'OpenDrive',
        'Yandex Disk', '4shared', 'GitHub', 'GitLab', 'Immich', 'PixelUnion', 'Google Photos', 'ImageKit',
        'Uploadcare', 'Cloudinary', 'Amazon Web Services (AWS)', 'Backblaze B2', 'Cloudflare R2', 'Wasabi',
        'IDrive e2', 'Filebase', 'Storj', 'DigitalOcean Spaces', 'Oracle Cloud', 'Alibaba OSS', 'Tencent COS',
        'FileLu S5', 'MEGA S4 Object Storage', 'MinIO', 'CloudMe', 'DriveHQ', 'Koofr (WebDAV)', 'Jianguoyun',
        'InfiniCloud', 'Seafile', 'FileLu WebDAV', 'Nextcloud', 'Felicloud', 'Hetzner Storage Box', 'Blomp', 'Drime',
    ],
    // The protocol id shown when a session has no provider.
    'src/components/SessionTabs.tsx': ['FTP'],
    // The otpauth URI scheme.
    'src/components/TotpSetup.tsx': ['otpauth URI:'],
    // The verifier library.
    'src/components/UpdateVerificationPanel.tsx': ['sigstore-rs'],
    // The Sigstore logo.
    'src/components/icons/SigstoreMark.tsx': ['Sigstore'],
    // The file extension and the cipher.
    'src/components/vault/VaultCreate.tsx': ['.aerovault · AES-256', 'zstd -3', 'zstd -9', 'zstd -15'],
    // Archive format and codec names.
    'src/components/CompressDialog.tsx': [
        'ZIP', 'AES-256 · Deflate', 'AES-256 · LZMA2', 'TAR', 'Gzip', 'XZ', 'TAR.BZ2', 'Bzip2', 'GZ', 'BZ2', 'LZMA2',
        'LZMA', 'PPMd', 'BZip2',
    ],
    // Image format names and extensions.
    'src/components/Preview/types.ts': ['PNG', 'JPEG', 'WebP', 'BMP', 'TIFF', 'GIF'],
    // Sponsor platforms and coin names.
    'src/components/SupportDialog.tsx': [
        'Bitcoin', 'BTC', 'Ethereum / EVM', 'ETH', 'Solana', 'SOL', 'Litecoin', 'LTC', 'GitHub Sponsors',
        'Buy Me a Coffee',
    ],
    // The names of the tools a profile is imported from.
    'src/components/bridge/bridgeSources.ts': [
        'WinSCP', 'FileZilla', 'AWS CLI', 'MinIO Client', 's3cmd', 'OpenSSH', 'PuTTY', 'MobaXterm', 'lftp',
        'Cyberduck', 'Dreamweaver', 'Kopia', 'restic', 'Duplicacy',
    ],
    // The petabyte unit.
    'src/components/common/ManualStorageField.tsx': ['PB'],
    // Product, protocol and format names.
    'src/components/providerModeGroups.tsx': [
        'API', 'WebDAV', 'FTP', 'CLI', 'WebDAV (MEGAcmd)', 'FileLu', 'Filen', 'Proton Drive', 'OpenDrive', 'Koofr', 'MEGA',
    ],
    // The provider a new connection tab is named after.
    'src/components/IntroHub/IntroHub.tsx': ['PixelUnion'],
    // Protocol class names.
    'src/types.ts': ['WebDAV', 'FTPS', 'FTP', 'SFTP', 'Azure', 'MTP', 'API', 'OAuth'],
    // Cipher names.
    'src/utils/archiveCipher.ts': ['AES', 'ZipCrypto'],
    // Font family names.
    'src/utils/fontPresets.ts': ['FiraGO', 'Noto Sans', 'JetBrains Mono'],
    // Connection class acronyms.
    'src/utils/providerConnectionMeta.ts': ['CMD', 'CLI', 'OAuth'],
    // Prism grammar names the code block shows.
    'src/components/DevTools/MarkdownRenderer.tsx': [
        'javascript', 'typescript', 'python', 'ruby', 'bash', 'yaml', 'docker', 'makefile', 'markup',
    ],
};

const squash = (text: string) => text.replace(/\s+/g, ' ').trim();

/** A module of the scanned sources: what it declares, imports and exports. */
type Module = {
    name: string;
    source: string;
    program: Node;
    top: Map<string, Node | null>;
    imports: Map<string, { from: string; imported: string }>;
    exports: Map<string, string>;
    reexports: Map<string, { from: string; imported: string }>;
    // useState setter name -> every value the state can hold: its initial value
    // and each value passed to the setter in the module.
    state: Map<string, Node[]>;
    // Identifier-like literals the module compares (`=== 'built'`, `case 'global':`).
    compared: Set<string>;
};

/**
 * A node together with the module it is written in. `embedded` marks a value
 * interpolated into a template literal, which reads as part of a sentence.
 * `scopes` is the scope an object or array literal is written in, so its values
 * resolve there and not at the caller: a record a function returns keeps the
 * function's declarations.
 */
type Found = { node: Node; mod: Module; embedded?: boolean; scopes?: Array<Map<string, Node | null>> };

/** Several values one name can stand for: the elements a callback walks, a state's values. */
const union = (values: Found[]): Node => ({ type: 'Union', start: 0, end: 0, values });

type Context = {
    mod: Module;
    scopes: Array<Map<string, Node | null>>;
    modules: Map<string, Module>;
    seen: Set<Node>;
    // A function's return value is read in the function's own scope, which is
    // not the caller's: its names are not followed.
    follow: boolean;
    // How many names deep the walk is: a chain of names that leads back to
    // itself stops here.
    depth: number;
};

const WRAPPERS = ['ParenthesizedExpression', 'TSAsExpression', 'TSNonNullExpression', 'TSSatisfiesExpression', 'ChainExpression'];

/** Array methods that return the same elements, and those that return one of them. */
const ARRAY_KEEP = new Set(['filter', 'slice', 'sort', 'toSorted', 'reverse', 'toReversed', 'concat']);
const ARRAY_PICK = new Set(['find', 'findLast', 'at']);
/** Array methods whose callback receives each element. */
const ARRAY_WALK = new Set(['map', 'flatMap', 'forEach', 'filter', 'find', 'findLast', 'some', 'every', 'sort', 'toSorted']);

const nameOf = (node: Node | null | undefined): string | undefined => {
    if (!node) return undefined;
    if (node.type === 'Identifier') return (node as unknown as { name: string }).name;
    if (node.type === 'Literal' && typeof node.value === 'string') return node.value;
    return undefined;
};

const isUseState = (node: Node | null | undefined) => {
    if (!node || node.type !== 'CallExpression') return false;
    const callee = node.callee as Node;
    return nameOf(callee) === 'useState' || (callee.type === 'MemberExpression' && nameOf(callee.property as Node) === 'useState');
};

function join(from: string, spec: string): string {
    const parts = from.split('/').slice(0, -1);
    for (const part of spec.split('/')) {
        if (part === '..') parts.pop();
        else if (part !== '.') parts.push(part);
    }
    return parts.join('/');
}

function indexModule(name: string, source: string, program: Node, names: Set<string>): Module {
    const mod: Module = {
        name, source, program, top: new Map(), imports: new Map(), exports: new Map(), reexports: new Map(), state: new Map(),
        compared: new Set(),
    };
    const target = (spec: string) => {
        if (!spec.startsWith('.')) return null;
        const base = join(name, spec);
        return [base, `${base}.tsx`, `${base}.ts`, `${base}/index.tsx`, `${base}/index.ts`].find((candidate) => names.has(candidate)) ?? null;
    };
    for (const statement of program.body as Node[]) {
        const from = isNode(statement.source) ? target((statement.source as Node).value as string) : null;
        if (statement.type === 'ImportDeclaration' && from) {
            for (const specifier of statement.specifiers as Node[]) {
                const local = nameOf(specifier.local as Node)!;
                if (specifier.type === 'ImportSpecifier') mod.imports.set(local, { from, imported: nameOf(specifier.imported as Node)! });
                if (specifier.type === 'ImportDefaultSpecifier') mod.imports.set(local, { from, imported: 'default' });
            }
        }
        if (statement.type === 'ExportNamedDeclaration') {
            for (const specifier of (statement.specifiers as Node[]) ?? []) {
                const exported = nameOf(specifier.exported as Node)!;
                const local = nameOf(specifier.local as Node)!;
                if (from) mod.reexports.set(exported, { from, imported: local });
                else if (!statement.source) mod.exports.set(exported, local);
            }
            const declaration = statement.declaration as Node | null;
            if (declaration?.type === 'FunctionDeclaration' && isNode(declaration.id)) mod.exports.set(nameOf(declaration.id)!, nameOf(declaration.id)!);
            if (declaration?.type === 'VariableDeclaration') {
                for (const declarator of declaration.declarations as Node[]) bindingNames(declarator.id as Node).forEach((local) => mod.exports.set(local, local));
            }
        }
        if (statement.type === 'ExportDefaultDeclaration') {
            const declaration = statement.declaration as Node;
            const local = nameOf(declaration) ?? nameOf(declaration.id as Node) ?? '*default*';
            if (local === '*default*') mod.top.set(local, declaration);
            mod.exports.set('default', local);
        }
    }
    // A state's values: the initial one and every one passed to its setter.
    const setters = new Map<string, Node[]>();
    const calls: Node[] = [];
    const walk = (node: Node) => {
        if (node.type === 'VariableDeclarator' && (node.id as Node).type === 'ArrayPattern' && isUseState(node.init as Node)) {
            const setter = nameOf(((node.id as Node).elements as Array<Node | null>)[1]);
            const initial = ((node.init as Node).arguments as Node[])[0];
            if (setter) setters.set(setter, [...(setters.get(setter) ?? []), ...(initial ? [initial] : [])]);
        }
        if (node.type === 'CallExpression' && (node.callee as Node).type === 'Identifier') calls.push(node);
        const operands = node.type === 'BinaryExpression' && ['===', '!==', '==', '!='].includes(node.operator as string)
            ? [node.left as Node, node.right as Node]
            : node.type === 'SwitchCase' && isNode(node.test) ? [node.test as Node] : [];
        for (const operand of operands) if (isIdentifierValue(operand)) mod.compared.add(literalText(operand)!);
        children(node).forEach(walk);
    };
    walk(program);
    for (const call of calls) {
        const values = setters.get(nameOf(call.callee as Node)!);
        const argument = (call.arguments as Node[])[0];
        if (!values || !argument) continue;
        values.push(...(isFunction(argument) ? returned(argument) : [argument]));
    }
    mod.state = setters;
    hoist(mod, mod.top, program.body as Node[]);
    return mod;
}

// A declaration is visible to the whole scope that holds it: a component calls
// a helper the module declares further down.
function hoist(mod: Module, scope: Map<string, Node | null>, statements: Node[]) {
    for (const statement of statements) {
        const declaration = statement.type.startsWith('Export') ? statement.declaration : statement;
        if (!isNode(declaration)) continue;
        if (declaration.type === 'FunctionDeclaration' && isNode(declaration.id)) scope.set(nameOf(declaration.id)!, declaration);
        if (declaration.type === 'VariableDeclaration') (declaration.declarations as Node[]).forEach((declarator) => declare(mod, scope, declarator));
    }
}

function declare(mod: Module, scope: Map<string, Node | null>, declarator: Node) {
    const id = declarator.id as Node;
    if (id.type === 'Identifier') {
        scope.set(nameOf(id)!, isNode(declarator.init) ? declarator.init : null);
        return;
    }
    bindingNames(id).forEach((name) => scope.set(name, null));
    if (id.type === 'ArrayPattern' && isUseState(declarator.init as Node)) {
        const [value, setter] = (id.elements as Array<Node | null>).map(nameOf);
        if (value && setter) scope.set(value, union((mod.state.get(setter) ?? []).map((node) => ({ node, mod }))));
    }
}

function lookup(name: string, ctx: Context): Found | null {
    const scope = [...ctx.scopes].reverse().find((candidate) => candidate.has(name));
    if (scope) {
        const node = scope.get(name);
        return node ? { node, mod: ctx.mod } : null;
    }
    return imported(ctx.modules, ctx.mod, name, 0);
}

function imported(modules: Map<string, Module>, mod: Module, name: string, depth: number): Found | null {
    const link = mod.imports.get(name);
    const target = link && modules.get(link.from);
    return target && depth < 8 ? exported(modules, target, link.imported, depth + 1) : null;
}

function exported(modules: Map<string, Module>, mod: Module, name: string, depth: number): Found | null {
    const local = mod.exports.get(name);
    if (local !== undefined) {
        const node = mod.top.get(local);
        return node ? { node, mod } : imported(modules, mod, local, depth);
    }
    const link = mod.reexports.get(name);
    const target = link && modules.get(link.from);
    return target && depth < 8 ? exported(modules, target, link.imported, depth + 1) : null;
}

/**
 * The scope a function's return values are read in: its parameters (no value)
 * and the declarations of its body, over the scope the function is declared in.
 */
function bodyScope(fn: Found, ctx: Context): Array<Map<string, Node | null>> {
    const scope = new Map<string, Node | null>();
    ((fn.node.params as Node[] | undefined) ?? []).forEach((param) => bindingNames(param).forEach((local) => scope.set(local, null)));
    const body = fn.node.body as Node;
    if (body.type === 'BlockStatement') hoist(fn.mod, scope, body.body as Node[]);
    return [...(fn.mod === ctx.mod ? ctx.scopes : [fn.mod.top]), scope];
}

const within = (found: Found, ctx: Context): Context =>
    found.scopes ? { ...ctx, mod: found.mod, scopes: found.scopes }
        : found.mod === ctx.mod ? ctx : { ...ctx, mod: found.mod, scopes: [found.mod.top] };

/** The object and array literals an expression can evaluate to. */
function objects(node: Node, ctx: Context, depth = 0): Found[] {
    if (depth > 8) return [];
    const again = (next: Node, c = ctx) => objects(next, c, depth + 1);
    if (node.type === 'ObjectExpression' || node.type === 'ArrayExpression') return [{ node, mod: ctx.mod, scopes: [...ctx.scopes] }];
    if (WRAPPERS.includes(node.type)) return again(node.expression as Node);
    if (node.type === 'ConditionalExpression') return [...again(node.consequent as Node), ...again(node.alternate as Node)];
    if (node.type === 'LogicalExpression') return [...(node.operator === '&&' ? [] : again(node.left as Node)), ...again(node.right as Node)];
    if (node.type === 'Union') return (node.values as Found[]).flatMap((value) => again(value.node, within(value, ctx)));
    if (!ctx.follow) return [];
    if (node.type === 'Identifier') {
        const found = lookup(nameOf(node)!, ctx);
        return found ? again(found.node, within(found, ctx)) : [];
    }
    if (node.type === 'MemberExpression') {
        return again(node.object as Node).flatMap((object) => properties(object, node, ctx, depth + 1)).flatMap((value) => again(value.node, within(value, ctx)));
    }
    if (node.type === 'CallExpression') {
        const callee = node.callee as Node;
        if (callee.type === 'Identifier') {
            const found = lookup(nameOf(callee)!, ctx);
            if (!found || !isFunction(found.node)) return [];
            const inner = { ...within(found, ctx), scopes: bodyScope(found, ctx) };
            return returned(found.node).flatMap((value) => again(value, inner));
        }
        if (callee.type === 'MemberExpression' && !callee.computed) {
            const method = nameOf(callee.property as Node)!;
            if (ARRAY_KEEP.has(method)) return again(callee.object as Node);
            if (ARRAY_PICK.has(method)) return elements(again(callee.object as Node), ctx, depth + 1).flatMap((value) => again(value.node, within(value, ctx)));
        }
    }
    return [];
}

/** The values of an object literal a member expression reads, or of all its properties. */
function properties(object: Found, member: Node | null, ctx: Context, depth = 0): Found[] {
    if (depth > 8) return [];
    const inner = within(object, ctx);
    const key = member && !member.computed ? nameOf(member.property as Node) : undefined;
    if (object.node.type === 'ArrayExpression') return member && !member.computed ? [] : elements([object], ctx, depth);
    if (isPayload(object.node)) return [];
    const out: Found[] = [];
    for (const property of object.node.properties as Node[]) {
        if (property.type !== 'SpreadElement' && KEY_NAME.test(nameOf(property.key as Node) ?? '') && isIdentifierValue(property.value as Node)) continue;
        if (property.type === 'SpreadElement') {
            objects(property.argument as Node, inner, depth + 1).forEach((spread) => out.push(...properties(spread, member, ctx, depth + 1)));
        } else if (key === undefined || nameOf(property.key as Node) === key) {
            out.push({ node: property.value as Node, mod: object.mod, scopes: object.scopes });
        }
    }
    return out;
}

function elements(arrays: Found[], ctx: Context, depth = 0): Found[] {
    if (depth > 8) return [];
    return arrays.filter((array) => array.node.type === 'ArrayExpression').flatMap((array) =>
        (array.node.elements as Array<Node | null>).filter(isNode).flatMap((element) =>
            element.type === 'SpreadElement'
                ? elements(objects(element.argument as Node, within(array, ctx), depth + 1), ctx, depth + 1)
                : [{ node: element, mod: array.mod, scopes: array.scopes }]));
}

/**
 * The string nodes a JSX child expression renders: the same cases as texts(),
 * plus a template literal with no `t()` in it, both its text and what its
 * interpolations render (one with `t()` is the template check above), the
 * operands of a `+` with no `t()` in it (one with `t()` is the concatenation
 * check above) and TypeScript wrappers. A name is followed to what it stands
 * for: a variable, a state, a parameter's default, a property of an object or
 * an array element, a callback's element, a function's return values, in this
 * module or in the one it is imported from.
 */
function rendered(node: Node, out: Found[], ctx: Context, embedded = false): void {
    if (ctx.depth > 12) return;
    const again = (next: Node, c = ctx) => rendered(next, out, c, embedded);
    const deeper = (found: Found, follow = ctx.follow): Context => ({ ...within(found, ctx), follow, depth: ctx.depth + 1 });
    if (literalText(node) !== null) out.push({ node, mod: ctx.mod, embedded });
    else if (node.type === 'TemplateLiteral' && !(node.expressions as Node[]).some(containsT)) {
        out.push({ node, mod: ctx.mod, embedded });
        (node.expressions as Node[]).forEach((expression) => rendered(expression, out, ctx, true));
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
    } else if (WRAPPERS.includes(node.type)) {
        again(node.expression as Node);
    } else if (node.type === 'Union') {
        (node.values as Found[]).forEach((value) => again(value.node, within(value, ctx)));
    } else if (!ctx.follow) {
        return;
    } else if (node.type === 'Identifier') {
        const found = lookup(nameOf(node)!, ctx);
        if (!found || ctx.seen.has(found.node)) return;
        ctx.seen.add(found.node);
        again(found.node, deeper(found));
    } else if (node.type === 'CallExpression') {
        const found = (node.callee as Node).type === 'Identifier' ? lookup(nameOf(node.callee as Node)!, ctx) : null;
        if (!found || !isFunction(found.node) || ctx.seen.has(found.node)) return;
        ctx.seen.add(found.node);
        // A return value is read in the function's own scope, not the caller's.
        const inner = { ...deeper(found), scopes: bodyScope(found, ctx) };
        returned(found.node).forEach((value) => again(value, inner));
    } else if (node.type === 'MemberExpression') {
        objects(node.object as Node, ctx)
            .flatMap((object) => properties(object, node, ctx))
            .forEach((value) => again(value.node, deeper(value)));
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
const PROSE_ATTRIBUTES = new Set(['title', 'placeholder', 'aria-label', 'aria-description', 'aria-valuetext', 'alt', 'label']);

const attributeName = (node: Node) => (node.name as { name?: unknown }).name;

/**
 * Names that hold text a reader sees, as a component prop or an object
 * property: `heading="Saved servers"`, `{ label: 'Fast', value: 'fast' }`.
 */
const TEXT_NAME = /^(?:label|title|text|message|details|description|placeholder|tooltip|hint|heading|header|caption|subtitle|summary|ariaLabel)$|[a-z](?:Label|Title|Text|Message|Description|Placeholder|Tooltip|Hint|Heading|Caption|Subtitle)$/;

/**
 * An object with a snake_case key mirrors a backend or runner payload
 * (`{ status, duration_ms, message }`): it is data, and its strings are not
 * the interface's text.
 */
const isPayload = (object: Node) =>
    object.type === 'ObjectExpression' && (object.properties as Node[]).some((property) => /^[a-z0-9]+_[a-z0-9_]+$/.test(nameOf(property.key as Node) ?? ''));

/** Properties whose identifier-like value is a data key: `{ name: 'path' }`, `{ id: 'builtin_edit' }`. */
const KEY_NAME = /^(?:name|id|key|type|kind|value|cmd|command|toolName|runner|format|family)$/;
const isIdentifierValue = (node: Node) => {
    const text = literalText(node);
    return text !== null && /^[a-z][a-z0-9]*(?:[_-][a-z0-9]+)*$/.test(text);
};

/**
 * Interpolation parameters whose value names an entity: a provider, a server,
 * an address, a protocol, a plan, a storage class, a shell, a command. Such a
 * value is a name or a code, not a sentence (`t('activity.x', { provider:
 * 'Box' })`).
 */
const NAME_PARAM = /^(?:provider|server|host|ip|protocol|backend|author|plan|shell|storageClass|tier|cmd|command)$/;

/** A placeholder marker a sentence is split on: `t('x', { cmd: '{cmd}' }).split('{cmd}')`. */
const isMarker = (text: string) => /^\{\w+\}$/.test(text.trim());

/**
 * Calls whose arguments never reach the interface: the console, the backend
 * (`invoke`) and serialization.
 */
const isPayloadCall = (call: Node) => {
    const callee = call.callee as Node;
    if (nameOf(callee) === 'invoke') return true;
    if (callee.type !== 'MemberExpression') return false;
    const object = nameOf(callee.object as Node);
    return object === 'console' || object === 'JSON' || nameOf(callee.property as Node) === 'invoke';
};

const isComponent = (opening: Node) => {
    const name = opening.name as Node;
    return name.type === 'JSXMemberExpression' || /^[A-Z]/.test(String((name as { name?: unknown }).name ?? ''));
};

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
    const names = new Set(Object.keys(sources).map((file) => file.replace(/^\//, '')));
    const modules = new Map<string, Module>();
    for (const [file, source] of Object.entries(sources)) {
        const name = file.replace(/^\//, '');
        const program = parseAst(source, { lang: file.endsWith('.tsx') ? 'tsx' : 'ts' }, file) as unknown as Node;
        modules.set(name, indexModule(name, source, program, names));
    }
    // A value the code compares anywhere is a data key, even where it is rendered.
    const compared = new Set([...modules.values()].flatMap((mod) => [...mod.compared]));
    const reportIn = (mod: Module, node: Node, start = node.start) => {
        const line = mod.source.slice(0, start).split('\n').length;
        const entry = `${mod.name}:${line} ${squash(mod.source.slice(start, node.end)).slice(0, 90)}`;
        if (!found.includes(entry)) found.push(entry);
    };
    const neutral = (text: string, file: string) => {
        if (!allowed[file]?.includes(squash(text))) return !isJsxProse(text);
        unused.delete(`${file} ${squash(text)}`);
        return true;
    };
    for (const mod of modules.values()) {
        const { name, program } = mod;
        const report = (node: Node, start = node.start) => reportIn(mod, node, start);
        // One map per lexical scope, innermost last. A name resolves to the closest
        // scope that declares it; a parameter shadows with no value (null), or with
        // its default value when it has one.
        const scopes: Array<Map<string, Node | null>> = [mod.top];
        const context = (): Context => ({ mod, scopes, modules, seen: new Set(), follow: true, depth: 0 });
        // A callback an array method calls receives each element of that array.
        const callbacks = new Map<Node, { values: Found[]; entries: boolean }>();
        const proseVariable = (node: Node) => {
            if (node.type !== 'Identifier') return false;
            const id = nameOf(node)!;
            const scope = [...scopes].reverse().find((candidate) => candidate.has(id));
            const init = scope?.get(id);
            return !!init && init.type !== 'Union' && texts(init).some(isProse);
        };
        const bindParameter = (scope: Map<string, Node | null>, param: Node, values: Found[] | null, entries: boolean) => {
            bindingNames(param).forEach((local) => scope.set(local, null));
            const ctx = context();
            const field = (key: string) => union((values ?? []).flatMap((value) =>
                objects(value.node, within(value, ctx)).flatMap((object) =>
                    properties(object, { type: 'MemberExpression', start: 0, end: 0, computed: false, property: { type: 'Identifier', name: key } } as unknown as Node, ctx))));
            if (param.type === 'Identifier' && values) scope.set(nameOf(param)!, union(values));
            if (param.type === 'AssignmentPattern' && (param.left as Node).type === 'Identifier') {
                scope.set(nameOf(param.left as Node)!, union([...(values ?? []), { node: param.right as Node, mod }]));
            }
            if (param.type === 'ArrayPattern' && entries && values) {
                const second = (param.elements as Array<Node | null>)[1];
                if (second?.type === 'Identifier') scope.set(nameOf(second)!, union(values));
            }
            if (param.type === 'ObjectPattern') {
                for (const property of param.properties as Node[]) {
                    if (property.type !== 'Property') continue;
                    const key = nameOf(property.key as Node);
                    const value = property.value as Node;
                    const local = value.type === 'AssignmentPattern' ? value.left as Node : value;
                    if (!key || local.type !== 'Identifier') continue;
                    const defaults = value.type === 'AssignmentPattern' ? [{ node: value.right as Node, mod }] : [];
                    if (values || defaults.length) scope.set(nameOf(local)!, union([...(values ? (field(key).values as Found[]) : []), ...defaults]));
                }
            }
        };
        // Inside the arguments of a payload call (the console, the backend) no
        // object property is the interface's text.
        let quiet = 0;
        const visit = (node: Node) => {
            if (isStyleElement(node)) return;
            if (node.type === 'CallExpression' && isPayloadCall(node)) {
                visit(node.callee as Node);
                quiet += 1;
                (node.arguments as Node[]).forEach(visit);
                quiet -= 1;
                return;
            }
            const opensScope = SCOPES.has(node.type);
            if (opensScope) {
                const scope = new Map<string, Node | null>();
                const walked = callbacks.get(node);
                ((node.params as Node[] | undefined) ?? []).forEach((param, index) =>
                    bindParameter(scope, param, index === 0 && walked ? walked.values : null, !!walked?.entries));
                if (node.type === 'CatchClause' && isNode(node.param)) bindingNames(node.param).forEach((local) => scope.set(local, null));
                scopes.push(scope);
            }
            if (node.type === 'BlockStatement' || node.type === 'StaticBlock') hoist(mod, scopes[scopes.length - 1], node.body as Node[]);
            if (node.type === 'VariableDeclarator') declare(mod, scopes[scopes.length - 1], node);
            if (node.type === 'CallExpression') {
                const callee = node.callee as Node;
                const callback = (node.arguments as Node[])[0];
                if (callee.type === 'MemberExpression' && !callee.computed && ARRAY_WALK.has(nameOf(callee.property as Node)!) && isFunction(callback)) {
                    const ctx = context();
                    const source = callee.object as Node;
                    const objectCall = source.type === 'CallExpression' && (source.callee as Node).type === 'MemberExpression'
                        && nameOf((source.callee as Node).object as Node) === 'Object' ? nameOf((source.callee as Node).property as Node) : undefined;
                    if (objectCall === 'values' || objectCall === 'entries') {
                        const values = objects((source.arguments as Node[])[0], ctx).flatMap((object) => properties(object, null, ctx));
                        callbacks.set(callback, { values, entries: objectCall === 'entries' });
                    } else {
                        callbacks.set(callback, { values: elements(objects(source, ctx), ctx), entries: false });
                    }
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
                // A name glued to a translation (`AES ${t('bits')}`) is allowed by
                // file and exact text, like the JSX it is rendered as.
                const glued = (text: string) => {
                    if (!allowed[name]?.includes(squash(text))) return isProse(text);
                    unused.delete(`${name} ${squash(text)}`);
                    return false;
                };
                if (quasis.map((quasi) => glued(quasi.value.cooked)).includes(true) || expressions.some(proseVariable)) {
                    report(node);
                }
            }
            // English the expression renders. A string written inside it reports
            // the expression; one reached through a name reports where it is
            // written, in whichever module. Every neutral() call runs, so each
            // allowance that matches is marked used.
            const english = (expression: Node, atString = false, embedded = false, ctx = context()) => {
                const strings: Found[] = [];
                rendered(expression, strings, ctx, embedded);
                let direct = false;
                for (const string of strings) {
                    if (!string.embedded && isIdentifierValue(string.node) && compared.has(literalText(string.node)!)) continue;
                    if (neutral(renderedText(string.node), string.mod.name)) continue;
                    const inside = string.mod === mod && string.node.start >= expression.start && string.node.end <= expression.end;
                    if (inside && !atString) direct = true;
                    else reportIn(string.mod, string.node);
                }
                return direct;
            };
            if ((node.type === 'JSXElement' || node.type === 'JSXFragment') && !CODE_ELEMENTS.has(elementName(node) ?? '')) {
                const kids = node.children as Node[];
                kids.forEach((kid, index) => {
                    const previous = kids[index - 1];
                    const extension = previous?.type === 'JSXText' && /\.$/.test(previous.value as string);
                    if (kid.type === 'JSXExpressionContainer' && !extension && english(kid.expression as Node)) report(kid);
                    if (kid.type === 'JSXText' && !neutral(kid.value as string, name)) {
                        report(kid, kid.start + (kid.value as string).search(/\S/));
                    }
                });
            }
            if (node.type === 'JSXOpeningElement') {
                for (const attribute of node.attributes as Node[]) {
                    if (attribute.type !== 'JSXAttribute' || !isNode(attribute.value)) continue;
                    const key = String(attributeName(attribute));
                    if (!PROSE_ATTRIBUTES.has(key) && !(isComponent(node) && TEXT_NAME.test(key))) continue;
                    const value = attribute.value as Node;
                    if (english(value.type === 'JSXExpressionContainer' ? value.expression as Node : value)) report(attribute);
                }
            }
            // A value passed to t() is interpolated into the translated text,
            // unless its parameter names an entity, whose value is a name. The
            // parameters may be a record declared elsewhere or spread into the
            // call; a value the code compares is still text here.
            if (isT(node) && isNode((node.arguments as Node[])[1]) && !quiet) {
                const ctx = context();
                const check = (record: Found, depth: number) => {
                    if (record.node.type !== 'ObjectExpression' || depth > 8) return;
                    const inner = within(record, ctx);
                    for (const property of record.node.properties as Node[]) {
                        if (property.type === 'SpreadElement') {
                            objects(property.argument as Node, inner).forEach((spread) => check(spread, depth + 1));
                        } else if (!NAME_PARAM.test(nameOf(property.key as Node) ?? '')) {
                            english(property.value as Node, true, true, inner);
                        }
                    }
                };
                objects((node.arguments as Node[])[1], ctx).forEach((record) => check(record, 0));
            }
            if (node.type === 'ObjectExpression' && isPayload(node)) {
                quiet += 1;
                children(node).forEach(visit);
                quiet -= 1;
                return;
            }
            if (node.type === 'Property' && !quiet && name.endsWith('.tsx') && !node.computed && TEXT_NAME.test(nameOf(node.key as Node) ?? '')) {
                english(node.value as Node, true);
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
                "sample.tsx:3 'Patch Dry Run Review'",
                "sample.tsx:3 'Patch Apply Review'",
                "sample.tsx:1 'would delete'",
                "sample.tsx:1 'deleted'",
                'sample.tsx:12 placeholder="Pair name"',
                "sample.tsx:21 'Declared further down'",
            ],
            unused: [],
        });
    });

    it('follows a rendered name to objects, states, props and other modules, and passes data by its shape', () => {
        const labels = [
            "export const FONT_PRESETS = [{ value: 'inter', label: 'Inter (Default)' }, { value: 'system', label: 'System Default' }];",
            "export const formatAgo = (seconds: number) => seconds < 60 ? 'just now' : `${seconds}s`;",
            "export { FONT_PRESETS as PRESETS };",
            "export function describeRatio(pct: number) { const verb = pct >= 0 ? 'saved' : 'grew'; return `${verb} ${pct}%`; }",
        ].join('\n');
        const sample = [
            "import { PRESETS, formatAgo, describeRatio } from './labels';",
            "const STATUS = { ok: { label: 'Healthy' }, down: { label: 'Unreachable' } };",
            "const checkState = (ok: boolean) => ok ? { label: 'passed' } : { label: 'failed' };",
            "const EXT = { plain: 'log', json: 'json' };",
            "function Panel({ heading = 'Untitled', ok, kind }: { heading?: string; ok: boolean; kind: 'ok' | 'down' }) {",
            "    const [error, setError] = useState('');",
            "    const state = checkState(ok);",
            "    const save = () => { setError('Invalid vault name'); console.log({ label: 'Debug only' }); invoke('save', { title: 'Payload' }); };",
            "    return (",
            "        <div>",
            "            <h2>{heading}</h2>",
            "            <span>{state.label} {STATUS[kind].label} {error}</span>",
            "            {PRESETS.map((preset) => <option key={preset.value}>{preset.label}</option>)}",
            "            <span>{formatAgo(5)}</span>",
            "            <span>{kind}.{EXT.plain}</span>",
            "            <Section heading=\"Saved servers\" variant=\"primary\" />",
            "            <span>{[{ name: 'path', type: 'string' }].map((p) => p.name)}</span>",
            "            <p>Use the key, e.g. the default one.</p>",
            "        </div>",
            "    );",
            "}",
            "const RESULT = { status: 'fail', duration_ms: 0, message: 'Unknown runner' };",
            "const OPTIONS = [{ label: '1 hour', value: 3600 }];",
            "const cipher = `AES ${t('introHub.cryptBadge.keyBits', { bits: 256 })}`;",
            "const glued = `Cipher ${t('introHub.cryptBadge.keyBits', { bits: 256 })}`;",
            "function Status({ wide }: { wide: boolean }) {",
            "    const mode = wide ? 'expanded' : 'compact';",
            "    if (mode === 'compact' || mode === 'expanded') log({ details: describeRatio(5) });",
            "    return <span>{mode} {'IT'}</span>;",
            "}",
            "const params = t('activity.x', { provider: 'Box', reason: 'Fatal error', link: '{link}', count: 2 });",
            "const record = { reason: 'Disk full' };",
            "const fromRecord = t('activity.x', record);",
            "const fromSpread = t('activity.x', { ...record, provider: 'Box' });",
            "if (status === 'stopped') log(status);",
            "const comparedWord = t('activity.x', { reason: 'stopped' });",
            "function failure() { const reason = 'Quota exceeded'; return { reason }; }",
            "const fromFunction = t('activity.x', failure());",
        ].join('\n');
        const result = scan({ 'labels.ts': labels, 'panel.tsx': sample }, { 'panel.tsx': ['AES'] });
        expect(result.unused).toEqual([]);
        expect(result.found).toEqual([
            "panel.tsx:2 'Healthy'",
            "panel.tsx:2 'Unreachable'",
            "panel.tsx:3 'passed'",
            "panel.tsx:3 'failed'",
            "panel.tsx:5 'Untitled'",
            "panel.tsx:8 'Invalid vault name'",
            "labels.ts:1 'Inter (Default)'",
            "labels.ts:1 'System Default'",
            "labels.ts:2 'just now'",
            'panel.tsx:16 heading="Saved servers"',
            'panel.tsx:18 Use the key, e.g. the default one.',
            "panel.tsx:23 '1 hour'",
            "panel.tsx:25 `Cipher ${t('introHub.cryptBadge.keyBits', { bits: 256 })}`",
            "labels.ts:4 'saved'",
            "labels.ts:4 'grew'",
            "panel.tsx:31 'Fatal error'",
            "panel.tsx:32 'Disk full'",
            "panel.tsx:36 'stopped'",
            "panel.tsx:37 'Quota exceeded'",
        ]);
    });

    // Parsing every source file takes about 3 s on an idle machine and over 10 s
    // under a load average of 30, past vitest's default 5 s.
    it('no source file of the app does', () => {
        expect(Object.keys(SOURCES).length).toBeGreaterThan(100);
        expect(scan(SOURCES, JSX_NEUTRAL)).toEqual({ found: [], unused: [] });
    }, 30_000);
});
