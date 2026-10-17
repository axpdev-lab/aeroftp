// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Reads the report of `appstreamcli validate --format yaml` into the findings
// that scripts/check-appstream.mjs decides on: { tag, severity, line? }.
// Only the fields the verdict reads are carried. `hint` and `explanation` are
// left out on purpose: the verdict reads neither, and they are not the same
// across versions even for the same file. Measured on an `xml-markup-invalid`
// hint that quotes an indented XML line: 1.1.2 drops that indentation, and
// PyYAML and the `yaml` package both read its document the same way, so the
// difference is in the emitter, not in the reader.
//
// The same flag produces two different documents depending on the version,
// both measured on this repository's metainfo:
//
//   1.0.2 (Ubuntu 24.04, the ubuntu-latest runner today): block YAML, one
//         `- tag: ...` record per finding, `Passed: yes|no` at the end.
//   1.1.2 (Ubuntu 26.04 LTS, emitted through libfyaml 0.9.4): a JSON-shaped
//         document, quoted keys, `"Issues": [ { "tag": ... } ]`,
//         `"Passed": "yes"|"no"`.
//
// The 1.1 document is NOT valid JSON, even though it looks like it. Its strings
// are YAML double-quoted scalars, and the emitter wraps them at about 80
// columns: with a bare line break (which YAML folds to a space), with a
// backslash before the break (which YAML joins with nothing), and with escapes
// JSON does not have. Measured: a hint that quotes the metainfo's own text, as
// `xml-markup-invalid` does, comes out with `\_` for a no-break space, `\L` for
// U+2028, `\U0001F600` for an emoji and `\a` for a bell. A plain JSON.parse
// throws on all of these, and the finding it throws on is exactly the real
// error the gate exists to name. So the strings are decoded by the YAML rules
// first and the structure, which is JSON, is then given to JSON.parse.
//
// Whatever cannot be read fails, with the reason. A reader that cannot see a
// finding must never answer "no findings".

const JSON_ESCAPES = new Set(['"', '\\', '/', 'b', 'f', 'n', 'r', 't']);

// YAML 1.2 escapes that JSON lacks, by the character that follows the backslash.
const YAML_ONLY_ESCAPES = {
  0: 0x00,
  a: 0x07,
  v: 0x0b,
  e: 0x1b,
  ' ': 0x20,
  '\t': 0x09,
  N: 0x85,
  _: 0xa0,
  L: 0x2028,
  P: 0x2029,
};

function jsonCodePoint(cp) {
  // String.fromCodePoint throws RangeError past U+10FFFF, which the caller
  // turns into a refusal rather than a guess.
  return JSON.stringify(String.fromCodePoint(cp)).slice(1, -1);
}

// Turns the body of one YAML double-quoted scalar into the body of a JSON
// string with the same value. Throws on an escape that neither language has.
export function yamlDoubleQuotedToJson(body) {
  return body.replace(
    /\\(?:\r?\n(?:[ \t]*\r?\n)*[ \t]*|x[0-9a-fA-F]{2}|u[0-9a-fA-F]{4}|U[0-9a-fA-F]{8}|[\s\S])|[ \t]*\r?\n(?:[ \t]*\r?\n)*[ \t]*|\t/g,
    (m) => {
      if (m === '\t') return '\\t';
      if (m[0] !== '\\') {
        // Line folding: one break between two lines is a space, and every
        // empty line after the first break is a newline. The white space
        // around the break is not content.
        const breaks = m.split('\n').length - 1;
        return breaks === 1 ? ' ' : '\\n'.repeat(breaks - 1);
      }
      const c = m[1];
      // An escaped line break joins the two lines with nothing, the
      // indentation of the continuation line is not content, and an empty
      // line after it is still a newline.
      if (/^\\\r?\n/.test(m)) return '\\n'.repeat(m.split('\n').length - 2);
      if ((c === 'x' && m.length === 4) || (c === 'U' && m.length === 10)) {
        return jsonCodePoint(parseInt(m.slice(2), 16));
      }
      if ((c === 'u' && m.length === 6) || JSON_ESCAPES.has(c)) return m;
      if (Object.hasOwn(YAML_ONLY_ESCAPES, c)) return jsonCodePoint(YAML_ONLY_ESCAPES[c]);
      throw new Error(`unknown escape ${JSON.stringify(m)} in a quoted string`);
    },
  );
}

function readFlowDocument(doc) {
  let parsed;
  try {
    const json = doc.replace(/"(?:[^"\\]|\\[\s\S])*"/g, (s) => `"${yamlDoubleQuotedToJson(s.slice(1, -1))}"`);
    parsed = JSON.parse(json);
  } catch (e) {
    return {
      error:
        `appstreamcli emitted a JSON-shaped report that does not parse (${e.message}). ` +
        'The report format changed: fix the parser, do not skip the gate.',
    };
  }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
    return { error: 'appstreamcli emitted a JSON-shaped report whose top level is not an object' };
  }
  // Every non-empty report captured carries an Issues list. A missing or null
  // one is a shape nobody has measured, and reading it as "no issues" would let
  // records under a renamed key pass as a clean report behind "Passed": "yes".
  const issues = parsed.Issues;
  if (!Array.isArray(issues)) {
    return {
      error: `appstreamcli emitted a JSON-shaped report whose Issues is ${issues === undefined ? 'missing' : 'not a list'}`,
    };
  }
  const findings = [];
  for (const [i, issue] of issues.entries()) {
    // A record without a tag or a severity cannot be judged, and one with a
    // line that is not a number cannot be matched against the metainfo.
    // Neither is skipped: skipping is how a reader reports zero findings.
    // `line` accepts what the block reader's `\d+` accepts, so the two
    // readers cannot disagree on it.
    if (
      !issue ||
      typeof issue.tag !== 'string' ||
      !issue.tag ||
      typeof issue.severity !== 'string' ||
      (issue.line !== undefined && !(Number.isInteger(issue.line) && issue.line >= 0))
    ) {
      return {
        error: `appstreamcli emitted issue ${i} in a shape this script does not read: ${JSON.stringify(issue)}`,
      };
    }
    const f = { tag: issue.tag, severity: issue.severity };
    if (issue.line !== undefined) f.line = issue.line;
    findings.push(f);
  }
  if (parsed.Passed !== 'yes' && parsed.Passed !== 'no') {
    return { error: 'appstreamcli emitted a JSON-shaped report without a "Passed": "yes" or "no" verdict' };
  }
  return { findings, passed: parsed.Passed === 'yes' };
}

function readBlockDocument(out) {
  const findings = [];
  let cur = null;
  for (const raw of out.split('\n')) {
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

  // A report with findings the parser cannot see is the dangerous case, so it
  // is named rather than left to the generic path. appstreamcli has been
  // observed emitting flow-style records (`{ tag: ..., severity: ... }`) in
  // other versions, which this block-style parser would skip. Measured: 1.0.2
  // emits block style for this file, for validate-tree, and for a numeric
  // component id, which is the shape that produced flow style in a reported
  // Debian case. So the case is not reproducible on that version, and the
  // script refuses to guess rather than silently reporting zero findings.
  // ANY flow-style record fails, not a majority of them. The first version of
  // this guard compared the two counts (`flowRecords > findings.length`), which
  // is a comparison between two populations where the question is simply "is
  // there even one record I cannot read". One block record next to one flow
  // record made that false, and an unapproved AppStream error inside the flow
  // one would have passed the gate: a guard written against silent failure,
  // failing silently.
  const flowRecords = (out.match(/^\s*[-{]?\s*\{\s*tag:/gm) ?? []).length;
  if (flowRecords > 0) {
    return {
      error:
        `appstreamcli emitted ${flowRecords} flow-style record(s) that this parser does not read ` +
        `(it read ${findings.length} block-style). The report format changed: fix the parser, do not skip the gate.`,
    };
  }
  // Every non-empty 1.0.2 report measured has a top-level `Issues:` key with
  // its records under it. Without that key, or with a bare `Issues:` in which
  // this parser read no record, `Passed: yes` would turn records it never saw
  // (a renamed key, an indented list) into a clean report. `Issues: []` is the
  // one empty list that reads as an answer.
  const issuesKey = /^Issues:[ \t]*(\[[ \t]*\])?[ \t]*$/m.exec(out);
  if (!issuesKey) {
    return { error: 'appstreamcli emitted a block report without a top-level Issues list' };
  }
  if (!issuesKey[1] && !findings.length) {
    return { error: 'appstreamcli emitted an Issues list in which this parser read no record' };
  }
  // `Passed:` is the last top-level key of every 1.0.2 report measured, so a
  // report without it may have been cut short after the records that were
  // read. Anchored to the start of a line, because an unanchored match would
  // also accept those words inside the text of a record.
  const verdict = /^Passed:\s*(yes|no)\s*$/m.exec(out);
  if (!verdict) {
    return { error: 'appstreamcli emitted a report without its closing `Passed:` verdict, so it may be cut short' };
  }
  return { findings, passed: verdict[1] === 'yes' };
}

// `stdout` is the validator's standard output alone. `status` is its exit code.
// Returns { findings, format } or { error }.
export function readValidateReport(stdout, status) {
  // Measured on both versions: a file with no finding at all, not even an
  // info or pedantic one, produces no output and exits 0. That is the one
  // empty report that is an answer; any other empty report is not.
  if (!stdout.trim()) {
    if (status === 0) return { findings: [], format: 'empty' };
    return { error: `appstreamcli exited ${status} with an empty report, so nothing was checked` };
  }
  const doc = stdout.replace(/^\s*---[ \t]*(?:\r?\n|$)/, '');
  // A YAML block document cannot begin with `{` or `[`: those open a flow
  // collection. So this is a choice by shape, not by trial, and a flow
  // document that will not parse is refused instead of being handed to the
  // block reader, which would read zero records from it.
  const flow = /^\s*[{[]/.test(doc);
  const r = flow ? readFlowDocument(doc) : readBlockDocument(stdout);
  if (r.error) return r;
  if (!r.findings.length && !r.passed) {
    return { error: 'appstreamcli produced a report this script could not read, so nothing was checked' };
  }
  return { findings: r.findings, format: flow ? 'json' : 'block' };
}
