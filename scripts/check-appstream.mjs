#!/usr/bin/env node
// Validate the AppStream metainfo and the desktop entries that the software
// centres read, and fail on anything this repository has not accepted with a
// written reason.
//
// Why this exists: nothing ran `appstreamcli` before, while the release
// procedure adds a <release> block on every version. An artefact that changes
// every release, that Ubuntu App Center and GNOME Software read, and that no
// gate looks at, is the same shape of hole as a lint nobody runs.
//
// Why it is not a plain `appstreamcli validate`: on this tree that exits 3
// today, so whoever added it would have seen red on the first run and removed
// it. The two findings are false positives of domain, measured one by one:
// they are URI schemes and a loopback address quoted as technical values
// inside prose, not links offered to the reader, which is what the rule
// exists to prevent.
//
// Why the exceptions are matched by CONTENT and never by line number: the
// metainfo holds 168 <release> blocks and the next release is inserted near
// the top, so every line number shifts at the next tag. An exception anchored
// to a line would silently stop matching the thing it was written for and
// start matching whatever moved into its place.

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';

const inCI = Boolean(process.env.GITHUB_ACTIONS);
const err = (m) => console.error(`${inCI ? '::error::' : ''}${m}`);

// A finding is accepted only when EVERY URI-looking token on the line it points
// at is one of these shapes, each with the reason it is not a link. Anything
// else of the same rule is still a failure, so a real https:// link added to a
// future release note is caught, including one sitting beside an allowed scheme.
//
// Each shape is matched against the PARSED url, not by string prefix, and a
// token that will not parse is never accepted: an exception that cannot say
// what it is looking at is a hole, not an exception.
// Matched against the parsed token, not by prefix: `startsWith('http://127.0.0.1')`
// also accepts http://127.0.0.1.example.invalid/, which is a domain that merely
// begins with those digits and is not loopback at all. Reproduced before fixing.
const PLAINTEXT_URL_ACCEPTED = [
  {
    label: 'aeroftp://',
    reason: 'the app own URI scheme, named as a resource identifier and not offered as a link',
    match: (t) => t.protocol === 'aeroftp:',
  },
  {
    label: 'tauri://',
    reason: 'the Tauri asset scheme, quoted while explaining a webview origin',
    match: (t) => t.protocol === 'tauri:',
  },
  {
    label: 'http://127.0.0.1',
    reason: 'a loopback address quoted as the value that a CVE fix reclassified',
    match: (t) => t.protocol === 'http:' && t.hostname === '127.0.0.1',
  },
];

// A token that cannot be parsed as a URL is never accepted: an exception that
// cannot say what it is looking at is not an exception, it is a hole.
function acceptedShape(raw) {
  let u;
  try {
    u = new URL(raw);
  } catch {
    return null;
  }
  return PLAINTEXT_URL_ACCEPTED.find((s) => s.match(u)) ?? null;
}

const METAINFO = 'app.aeroftp.AeroFTP.metainfo.xml';
// Desktop entries the stores and menus read. snap/gui/aeroftp.desktop is
// deliberately NOT here: it fails today for a different reason (a relative
// Icon path) and that is its own entry, not something to smuggle in behind
// this gate.
const DESKTOP = ['app.aeroftp.AeroFTP.desktop'];

function run(cmd, args) {
  try {
    return { out: execFileSync(cmd, args, { encoding: 'utf8' }), code: 0 };
  } catch (e) {
    if (e.code === 'ENOENT') return { missing: true };
    return { out: `${e.stdout ?? ''}${e.stderr ?? ''}`, code: e.status ?? 1 };
  }
}

// ---- metainfo -------------------------------------------------------------

if (!existsSync(METAINFO)) {
  err(`${METAINFO} is missing: this gate cannot answer, which is not the same as passing`);
  process.exit(1);
}

const res = run('appstreamcli', ['validate', '--no-net', '--format', 'yaml', METAINFO]);
if (res.missing) {
  err('appstreamcli is not installed, so this gate did not run. Install appstream, do not skip.');
  process.exit(1);
}

// The YAML report is parsed rather than the text one because it carries
// `severity` as a field. The exit code does not: measured on this tree, it is
// 3 both for two warnings and for a real error, so it cannot tell them apart.
const findings = [];
let cur = null;
for (const raw of res.out.split('\n')) {
  const line = raw.trimEnd();
  const m = /^-\s+tag:\s*(\S+)/.exec(line);
  if (m) {
    if (cur) findings.push(cur);
    cur = { tag: m[1] };
    continue;
  }
  if (!cur) continue;
  const s = /^\s+severity:\s*(\S+)/.exec(line);
  if (s) cur.severity = s[1];
  const l = /^\s+line:\s*(\d+)/.exec(line);
  if (l) cur.line = Number(l[1]);
}
if (cur) findings.push(cur);

// A report with findings the parser cannot see is the dangerous case, so it is
// named rather than left to the generic path. appstreamcli has been observed
// emitting flow-style records (`{ tag: ..., severity: ... }`) in other versions,
// which this block-style parser would skip. Measured here: 1.0.2 emits
// block style for this file, for validate-tree, and for a numeric component id,
// which is the shape that produced flow style in a reported Debian case. So the
// case is not reproducible on this version, and the script refuses to guess
// rather than silently reporting zero findings.
const flowRecords = (res.out.match(/^\s*[-{]?\s*\{\s*tag:/gm) ?? []).length;
if (flowRecords && flowRecords > findings.length) {
  err(
    `appstreamcli emitted ${flowRecords} flow-style record(s) that this parser does not read ` +
      `(it found ${findings.length}). The report format changed: fix the parser, do not skip the gate.`,
  );
  console.error(res.out.split('\n').slice(0, 20).join('\n'));
  process.exit(1);
}

if (!findings.length && !/Passed:\s*yes/.test(res.out)) {
  err('appstreamcli produced a report this script could not read, so nothing was checked');
  console.error(res.out.split('\n').slice(0, 20).join('\n'));
  process.exit(1);
}

const src = readFileSync(METAINFO, 'utf8').split('\n');
const blocking = [];
const accepted = [];

for (const f of findings) {
  if (f.severity === 'pedantic' || f.severity === 'info') continue;
  if (f.tag === 'description-has-plaintext-url' && f.line) {
    const text = src[f.line - 1] ?? '';
    // Every URI-looking token on the line must be an accepted shape, not just
    // one of them. Checking "does the line contain an accepted shape" lets a
    // real link ride along beside a scheme that is allowed: measured, an
    // injected https://example.com/docs next to aeroftp:// passed the gate.
    const tokens = (text.match(/[a-z][a-z0-9+.-]*:\/\/[^\s<)`"]*/gi) ?? []).map((t) =>
      // Trailing punctuation belongs to the prose, not to the URL.
      t.replace(/[.,;:)\]}'"`]+$/, ''),
    );
    const unexplained = tokens.filter((t) => !acceptedShape(t));
    if (tokens.length && !unexplained.length) {
      const shapes = [...new Set(tokens.map((t) => acceptedShape(t).label))];
      accepted.push(`${METAINFO}:${f.line} ${f.tag} (${shapes.join(', ')})`);
      continue;
    }
    if (unexplained.length) {
      blocking.push(
        `${METAINFO}:${f.line} ${f.severity} ${f.tag}: ${unexplained.join(', ')}`,
      );
      continue;
    }
  }
  blocking.push(`${METAINFO}:${f.line ?? '?'} ${f.severity} ${f.tag}`);
}

// ---- desktop entries ------------------------------------------------------

for (const d of DESKTOP) {
  if (!existsSync(d)) {
    blocking.push(`${d}: missing`);
    continue;
  }
  const r = run('desktop-file-validate', [d]);
  if (r.missing) {
    err('desktop-file-validate is not installed, so this gate did not run. Install desktop-file-utils.');
    process.exit(1);
  }
  for (const l of (r.out || '').split('\n')) {
    if (/:\s*error:/.test(l)) blocking.push(l.trim());
  }
}

// ---- verdict --------------------------------------------------------------

for (const a of accepted) console.log(`accepted  ${a}`);

if (blocking.length) {
  err('AppStream or desktop metadata does not validate:');
  for (const b of blocking) console.error(`  ${b}`);
  console.error(
    'Fix it, or, if the finding is a URI scheme or a value quoted as text rather than a link, ' +
      'add its shape to PLAINTEXT_URL_ACCEPTED in scripts/check-appstream.mjs with the reason.',
  );
  process.exit(1);
}

console.log(
  `appstream  ${METAINFO} and ${DESKTOP.length} desktop entry validate ` +
    `(${accepted.length} accepted finding${accepted.length === 1 ? '' : 's'}, listed above)`,
);
