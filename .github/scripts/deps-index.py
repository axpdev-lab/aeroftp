#!/usr/bin/env python3
"""AeroFTP dependency index: one run, every direct dependency, measured.

Reads the resolved graph (`cargo metadata`, `package-lock.json`), asks the
registries for what exists (crates.io sparse index, npm registry), and writes a
JSON file plus a Markdown table that answer, per direct dependency:

  requirement in the manifest | locked version | newest version the requirement
  already admits (`cargo update` / `npm update` reach it) | newest stable |
  status | held by dependabot | shown in the in-app Dependencies panel

Status values:
  ok            locked == newest stable
  compatible    a newer version fits the requirement: lockfile-only update
  incompatible  newest stable needs a manifest edit (semver-major for Cargo:
                a 0.x minor counts as major)
  pinned        exact `=` requirement and a newer version exists
  registry-err  the registry did not answer: NOT the same as ok

Usage: deps-index.py <repo-root> <out-dir>
Stdlib only. Network: index.crates.io and registry.npmjs.org (static/CDN, no
crawler rate limit, unlike the crates.io web API).
"""
import json
import os
import re
import subprocess
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

UA = "aeroftp-deps-index (https://github.com/axpdev-lab/aeroftp)"


# ---------------------------------------------------------------- semver ----
def parse_ver(v):
    v = v.split("+", 1)[0]
    pre = "-" in v
    core = v.split("-", 1)[0]
    parts = [int(x) for x in re.findall(r"\d+", core)[:3]]
    while len(parts) < 3:
        parts.append(0)
    return tuple(parts), pre


def _one_req(op, spec):
    nums = [int(x) for x in spec.split(".") if x.isdigit()]
    n = len(nums)
    base = tuple(nums + [0] * (3 - n))
    if op in ("", "^"):
        if base[0] > 0 or n == 1:
            upper = (base[0] + 1, 0, 0)
        elif base[1] > 0 or n == 2:
            upper = (0, base[1] + 1, 0)
        else:
            upper = (0, 0, base[2] + 1)
        return lambda t: base <= t < upper
    if op == "~":
        upper = (base[0] + 1, 0, 0) if n == 1 else (base[0], base[1] + 1, 0)
        return lambda t: base <= t < upper
    if op == "=":
        if n == 3:
            return lambda t: t == base
        upper = (base[0] + 1, 0, 0) if n == 1 else (base[0], base[1] + 1, 0)
        return lambda t: base <= t < upper
    if op == ">=":
        return lambda t: t >= base
    if op == ">":
        return lambda t: t > base
    if op == "<=":
        return lambda t: t <= base
    if op == "<":
        return lambda t: t < base
    if op == "*":
        return lambda t: True
    raise ValueError(op)


def req_matcher(req):
    req = req.strip()
    if req in ("", "*"):
        return lambda t: True
    preds = []
    for part in req.split(","):
        part = part.strip()
        m = re.match(r"^(\^|~|=|>=|<=|>|<)?\s*([0-9][0-9A-Za-z.\-+]*)$", part)
        if not m:
            raise ValueError(f"unparsed requirement {req!r}")
        preds.append(_one_req(m.group(1) or "", m.group(2).split("-")[0]))
    return lambda t: all(p(t) for p in preds)


def npm_matcher(req):
    req = req.strip()
    if req.startswith("$") or req in ("*", "latest", ""):
        return lambda t: True
    return req_matcher(req)  # ^ ~ = ranges used by this repo share Cargo's meaning


# -------------------------------------------------------------- registries --
def http_get(url, accept=None):
    r = urllib.request.Request(url, headers={"User-Agent": UA, **({"Accept": accept} if accept else {})})
    with urllib.request.urlopen(r, timeout=30) as resp:
        return resp.read().decode()


def sparse_path(name):
    n = name.lower()
    if len(n) == 1:
        return f"1/{n}"
    if len(n) == 2:
        return f"2/{n}"
    if len(n) == 3:
        return f"3/{n[0]}/{n}"
    return f"{n[:2]}/{n[2:4]}/{n}"


def crates_versions(name):
    body = http_get(f"https://index.crates.io/{sparse_path(name)}")
    out = []
    for line in body.splitlines():
        e = json.loads(line)
        t, pre = parse_ver(e["vers"])
        out.append({"v": e["vers"], "t": t, "pre": pre, "yanked": e.get("yanked", False),
                    "msrv": e.get("rust_version")})
    return out


def npm_versions(name):
    body = json.loads(http_get(f"https://registry.npmjs.org/{name.replace('/', '%2F')}",
                               "application/vnd.npm.install-v1+json"))
    out = []
    for v in body.get("versions", {}):
        t, pre = parse_ver(v)
        out.append({"v": v, "t": t, "pre": pre, "yanked": False, "msrv": None})
    return out, body.get("dist-tags", {}).get("latest")


def pick(versions, match=None):
    c = [x for x in versions if not x["pre"] and not x["yanked"] and (match is None or match(x["t"]))]
    return max(c, key=lambda x: x["t"]) if c else None


def classify(req, locked, latest_ok, latest):
    if latest is None:
        return "registry-err"
    lt, _ = parse_ver(locked)
    if lt >= latest["t"]:
        return "ok"
    if req.strip().startswith("="):
        return "pinned"
    if latest_ok and latest_ok["t"] > lt:
        return "compatible"
    return "incompatible"


# ------------------------------------------------------------------ inputs --
def dependabot_holds(repo):
    path = os.path.join(repo, ".github/dependabot.yml")
    holds, eco = {}, None
    if not os.path.exists(path):
        return holds
    for line in open(path):
        m = re.match(r'\s*- package-ecosystem:\s*"?([\w-]+)"?', line)
        if m:
            eco = m.group(1)
            holds.setdefault(eco, {})
            continue
        m = re.match(r'\s*- dependency-name:\s*"([^"]+)"', line)
        if m and eco:
            cur = m.group(1)
            holds[eco][cur] = []
            continue
        m = re.match(r'\s*(versions|update-types):\s*\[(.*)\]', line)
        if m and eco and holds[eco]:
            last = list(holds[eco])[-1]
            holds[eco][last].append(f"{m.group(1)}: {m.group(2)}")
    return holds


def panel_names(repo):
    src = open(os.path.join(repo, "src-tauri/build.rs")).read()
    m = re.search(r"TRACKED_DEPS[^=]*=\s*&\[(.*?)\];", src, re.S)
    if m:  # hand-written list, before the panel was derived from the manifests
        return set(re.findall(r'"([^"]+)"', m.group(1))), "build.rs TRACKED_DEPS (hand-written)"
    m = re.search(r"DEPENDENCY_CATEGORIES[^=]*=\s*&\[(.*?)\n\];", src, re.S)
    if m:  # derived list: build.rs fails on an unclassified crate, so the table is the panel
        categories = set(re.findall(r'\(\s*"([^"]+)",\s*&\[', m.group(1)))
        return set(re.findall(r'"([^"]+)"', m.group(1))) - categories, "build.rs DEPENDENCY_CATEGORIES (derived)"
    return None, "not found in build.rs"


def cargo_rows(repo):
    tauri_dir = os.path.join(repo, "src-tauri")
    meta = json.loads(subprocess.run(
        ["cargo", "metadata", "--format-version", "1", "--locked"], cwd=tauri_dir,
        check=True, capture_output=True, text=True).stdout)
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [p for p in meta["packages"] if p["source"] is None and p["name"] in ("aeroftp", "aeroftp-peer-l0")]
    rows = []
    for root in roots:
        node = nodes[root["id"]]
        for d in root["dependencies"]:
            if d["kind"] == "dev" or d["source"] is None and d["name"].startswith("aeroftp"):
                continue
            extern = (d["rename"] or d["name"]).replace("-", "_")
            cand = [pkgs[x["pkg"]] for x in node["deps"] if x["name"] == extern]
            if not cand:
                cand = [pkgs[x["pkg"]] for x in node["deps"] if pkgs[x["pkg"]]["name"] == d["name"]]
            locked = cand[0]["version"] if cand else None
            rows.append({"eco": "cargo", "root": root["name"], "name": d["name"], "rename": d["rename"],
                         "kind": d["kind"] or "normal", "target": d["target"], "optional": d["optional"],
                         "req": d["req"], "locked": locked})
    lock_pkgs = [p for p in meta["packages"] if p["source"]]
    by_name = {}
    for p in lock_pkgs:
        by_name.setdefault(p["name"], []).append(p["version"])
    msrv = sorted(((parse_ver(p["rust_version"])[0], p["name"], p["version"], p["rust_version"])
                   for p in lock_pkgs if p.get("rust_version")), reverse=True)
    declared = next(p.get("rust_version") for p in roots if p["name"] == "aeroftp")
    stats = {"lock_packages": len(lock_pkgs),
             "duplicated_names": {k: sorted(v) for k, v in sorted(by_name.items()) if len(v) > 1},
             "declared_rust_version": declared,
             "graph_msrv_floor": [{"crate": n, "version": v, "rust_version": r} for _, n, v, r in msrv[:15]],
             "crates_above_declared": sum(1 for t, *_ in msrv if declared and t > parse_ver(declared)[0])}
    return rows, stats


def npm_rows(repo):
    lock = json.load(open(os.path.join(repo, "package-lock.json")))
    root = lock["packages"][""]
    rows = []
    for kind in ("dependencies", "devDependencies"):
        for n, req in sorted(root.get(kind, {}).items()):
            rows.append({"eco": "npm", "root": "aeroftp", "name": n, "rename": None,
                         "kind": "normal" if kind == "dependencies" else "dev", "target": None,
                         "optional": False, "req": req,
                         "locked": lock["packages"].get(f"node_modules/{n}", {}).get("version")})
    return rows, {"lock_packages": len([k for k in lock["packages"] if k])}


# -------------------------------------------------------------------- main --
def main():
    repo, out = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
    os.makedirs(out, exist_ok=True)
    head = subprocess.run(["git", "rev-parse", "--short=9", "HEAD"], cwd=repo, capture_output=True, text=True).stdout.strip()
    crows, cstats = cargo_rows(repo)
    nrows, nstats = npm_rows(repo)
    holds = dependabot_holds(repo)
    panel, panel_src = panel_names(repo)

    def enrich(row):
        try:
            if row["eco"] == "cargo":
                vers = crates_versions(row["name"])
                match = req_matcher(row["req"])
            else:
                vers, _ = npm_versions(row["name"])
                match = npm_matcher(row["req"])
            latest, ok = pick(vers), pick(vers, match)
            row["latest"] = latest["v"] if latest else None
            row["latest_msrv"] = latest["msrv"] if latest else None
            row["latest_compatible"] = ok["v"] if ok else None
            row["status"] = classify(row["req"], row["locked"] or "0.0.0", ok, latest)
        except Exception as e:  # an unanswered registry is a result, not an ok
            row.update(latest=None, latest_compatible=None, latest_msrv=None, status="registry-err", error=str(e))
        eco = "cargo" if row["eco"] == "cargo" else "npm"
        row["dependabot_hold"] = holds.get(eco, {}).get(row["name"])
        row["in_panel"] = None if row["eco"] == "npm" or panel is None else row["name"] in panel
        return row

    with ThreadPoolExecutor(8) as ex:
        rows = list(ex.map(enrich, crows + nrows))

    report = {"repo_head": head, "cargo": cstats, "npm": nstats, "panel_source": panel_src, "rows": rows}
    json.dump(report, open(os.path.join(out, "deps-index.json"), "w"), indent=1)

    counts = {}
    for r in rows:
        counts.setdefault(r["eco"], {}).setdefault(r["status"], 0)
        counts[r["eco"]][r["status"]] += 1
    md = [f"# AeroFTP dependency index @ `{head}`", "",
          f"- Cargo: {len([r for r in rows if r['eco']=='cargo'])} direct deps, {cstats['lock_packages']} locked packages, "
          f"{len(cstats['duplicated_names'])} crate names locked in more than one version",
          f"- Declared rust-version {cstats['declared_rust_version']}; locked crates declaring a higher one: {cstats['crates_above_declared']}",
          f"- npm: {len([r for r in rows if r['eco']=='npm'])} direct deps, {nstats['lock_packages']} locked packages",
          f"- Status counts: {json.dumps(counts)}",
          f"- Panel source: {panel_src}", ""]
    if panel is not None:
        missing = sorted({r["name"] for r in rows if r["eco"] == "cargo" and r["kind"] == "normal" and not r["in_panel"]})
        extra = sorted(panel - {r["name"] for r in rows if r["eco"] == "cargo"})
        md += [f"- Direct Cargo deps NOT in the panel ({len(missing)}): {', '.join(missing) or 'none'}",
               f"- Panel entries that are not direct deps ({len(extra)}): {', '.join(extra) or 'none'}", ""]
    md += ["| eco | root | crate | rename | kind/target | req | locked | latest compatible | latest | latest MSRV | status | dependabot hold | panel |",
           "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    order = {"pinned": 0, "incompatible": 1, "compatible": 2, "registry-err": 3, "ok": 4}
    for r in sorted(rows, key=lambda r: (r["eco"], order.get(r["status"], 9), r["name"])):
        kt = r["kind"] + (f" `{r['target']}`" if r["target"] else "") + (" opt" if r["optional"] else "")
        hold = "; ".join(r["dependabot_hold"]) if r["dependabot_hold"] else ""
        panel_cell = "" if r["in_panel"] is None else ("yes" if r["in_panel"] else "**no**")
        md.append(f"| {r['eco']} | {r['root']} | {r['name']} | {r['rename'] or ''} | {kt} | `{r['req']}` | {r['locked']} | "
                  f"{r['latest_compatible'] or ''} | {r['latest'] or ''} | {r['latest_msrv'] or ''} | {r['status']} | {hold} | {panel_cell} |")
    md += ["", "## Highest rust-version declared by a locked crate", ""]
    md += [f"- {x['crate']} {x['version']}: rust {x['rust_version']}" for x in cstats["graph_msrv_floor"]]
    open(os.path.join(out, "deps-index.md"), "w").write("\n".join(md) + "\n")
    errs = [r["name"] for r in rows if r["status"] == "registry-err"]
    print(f"head={head} rows={len(rows)} counts={json.dumps(counts)} registry_errors={errs}")
    sys.exit(1 if errs else 0)


if __name__ == "__main__":
    main()
