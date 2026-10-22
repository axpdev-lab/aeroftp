#!/usr/bin/env python3
# Stand-in for the `proton-drive` CLI, used by `cli_sequence_tests` in
# src/providers/proton.rs. It records argv and fakes just enough behaviour for
# the provider sequences under test. It is a checked-in executable on purpose:
# see `link_shim` in that module for why the tests must not write it themselves.
import glob, json, os, pathlib, sys, time

# State lives next to the path this script was invoked through. The tests
# reach it through a per-test symlink, so that path is the symlink, never the
# checked-in file: do not resolve it.
HERE = os.path.dirname(os.path.abspath(sys.argv[0]))
LOG = os.path.join(HERE, "argv.log")
TRASH_JSON = os.path.join(HERE, "trash.json")
args = sys.argv[1:]
with open(LOG, "a") as f:
    f.write(json.dumps(args) + "\n")
# Which AeroFTP variables reached this child: none should.
with open(os.path.join(HERE, "env.log"), "a") as f:
    f.write(json.dumps(sorted(k for k in os.environ if k.startswith("AEROFTP_"))) + "\n")
verb = args[0] if args else ""
# Stand-in for a CLI that is installed but has no session: every command
# answers the way proton-drive does before `auth login`. A marker file next to
# the per-test link, not an environment variable, so parallel tests never see it.
if os.path.exists(os.path.join(HERE, "signed_out")):
    print("Error: You need to login first. Run `proton-drive auth login`.", file=sys.stderr)
    sys.exit(1)
sub = args[1] if len(args) > 1 else ""
# proton-drive reads a local path holding * ? [ or { as a glob, the upload
# sources and the download folder alike
# (cli/src/commands/fileSystem/localPath.ts); a pattern that matches nothing
# fails before any transfer.
def resolve_local(path):
    if not any(c in path for c in "*?[{"):
        return [path]
    matches = sorted(glob.glob(path))
    if not matches:
        print("No paths matched: %s" % path, file=sys.stderr)
        sys.exit(1)
    return matches
# A blocking native transfer for cancellation/drop tests.
sleep_marker = pathlib.Path(HERE, "transfer-sleep")
if verb == "filesystem" and sub in ("upload", "download") and sleep_marker.exists():
    pathlib.Path(HERE, "transfer.pid").write_text(str(os.getpid()))
    time.sleep(float(sleep_marker.read_text()))

if verb == "filesystem" and sub in ("upload", "download"):
    positional, rest = [], iter(range(2, len(args)))
    for i in rest:
        if args[i] in ("-f", "-d"):
            next(rest, None)
        elif not args[i].startswith("-"):
            positional.append(i)
    if sub == "upload":
        for i in positional[:-1]:
            resolve_local(args[i])
    elif positional:
        folders = resolve_local(args[positional[-1]])
        if len(folders) != 1:
            print("Expected exactly one local path", file=sys.stderr)
            sys.exit(1)
        args[positional[-1]] = folders[0]


def emit_transfer_failure(failures):
    if "-j" in args or "--json" in args:
        print(json.dumps({"transferredItems": 0, "transferredBytes": 0, "skippedItems": 0,
                          "failedItems": len(failures), "failures": failures}))
    else:
        print("Transfer summary:")
        for item in failures:
            print("  - %s: %s" % (item["name"], item["error"]))
        if sub == "upload" and any("quota" in item["error"].lower() for item in failures):
            with open(os.path.join(HERE, "browser.log"), "a") as f:
                f.write("upsell\n")
    print("%d item(s) failed to %s" % (len(failures), sub), file=sys.stderr)
    sys.exit(1)
# A transfer that fails once, then succeeds: the provider retries a transient
# item error (a timeout, a 429) and must not treat the first summary as final.
once = pathlib.Path(HERE, "transfer-fails-once.json")
if verb == "filesystem" and sub in ("upload", "download") and once.exists():
    failures = json.loads(once.read_text())
    once.unlink()
    emit_transfer_failure(failures)
# A transfer that fails for its items, the way proton-drive 0.9.0 reports it:
# the summary on stdout (JSON with -j), "N item(s) failed" on stderr, exit 1.
# Without -j a failure on a full drive also opens the upgrade page in the
# browser (cli/src/commands/fileSystem/storageQuotaUpsell.ts), which this
# stand-in records in browser.log instead.
failing = pathlib.Path(HERE, "transfer-fails.json")
if verb == "filesystem" and sub in ("upload", "download") and failing.exists():
    emit_transfer_failure(json.loads(failing.read_text()))
bulk_summary_file = pathlib.Path(HERE, "bulk-summary.json")
bulk_summary = json.loads(bulk_summary_file.read_text()) if bulk_summary_file.exists() else None
# The same summary for the first transfer only: the item retried afterwards on
# the single-file path then succeeds.
bulk_summary_once = pathlib.Path(HERE, "bulk-summary-once.json")
if verb == "filesystem" and sub in ("upload", "download") and bulk_summary_once.exists():
    bulk_summary = json.loads(bulk_summary_once.read_text())
    bulk_summary_once.unlink()
if verb == "filesystem" and sub == "download":
    dest = pathlib.Path(args[-1])
    failed_names = {item["name"] for item in (bulk_summary or {}).get("failures", [])}
    for i in positional[:-1]:
        remote = args[i]
        uid_map_file = pathlib.Path(HERE, "uid-name-map.json")
        if uid_map_file.exists():
            remote = json.loads(uid_map_file.read_text()).get(pathlib.Path(remote).name, remote)
        name = "".join("_" if ord(c) < 32 or ord(c) == 127 or c in '<>:"|?*\\' else c
                       for c in pathlib.Path(remote).name)
        if pathlib.Path(remote).name in failed_names:
            continue
        dest.mkdir(parents=True, exist_ok=True)
        target = dest / name
        if "-f" in args and args[args.index("-f") + 1] == "remove" and target.exists():
            target.unlink()
        target.write_text("REMOTE-CONTENT")
    if bulk_summary is not None:
        print(json.dumps(bulk_summary))
        # transfer-exit-1: a run that stopped resolving its sources, zero
        # counters and exit 1 (commandFileSystemDownload.ts).
        failed = bulk_summary.get("failedItems", 0) or pathlib.Path(HERE, "transfer-exit-1").exists()
        sys.exit(1 if failed else 0)
    print(json.dumps({"transferredItems": len(positional) - 1, "transferredBytes": 0,
                      "skippedItems": 0, "failedItems": 0, "failures": []}))
    sys.exit(0)
if verb == "filesystem" and sub == "upload":
    print(json.dumps(bulk_summary if bulk_summary is not None else {
        "transferredItems": len(positional) - 1, "transferredBytes": 0,
        "skippedItems": 0, "failedItems": 0, "failures": []}))
    sys.exit(1 if bulk_summary and bulk_summary.get("failedItems", 0) else 0)
if verb == "filesystem" and sub == "info":
    path = next((a for a in args[2:] if not a.startswith("-")), "/x")
    name = pathlib.Path(path).name
    # A test that lists the paths that exist (existing.json next to the link)
    # gets "not found" for every other one; without the list, every path
    # exists.
    existing = pathlib.Path(HERE, "existing.json")
    if existing.exists() and path not in json.loads(existing.read_text()):
        print("Error: %s not found" % path, file=sys.stderr)
        sys.exit(1)
    print(json.dumps({
        "name": {"ok": True, "value": name},
        "uid": "UID-CAPTURED",
        "type": "file",
        "path": path
    }))
    sys.exit(0)
if verb == "filesystem" and sub == "list":
    listed = next((a for a in args[2:] if not a.startswith("-")), "/")
    # What proton-drive 0.8.0 answers for the photo sections it cannot open
    # (measured 2026-09-22).
    if listed.rstrip("/") in ("/photos", "/albums"):
        print("Error: Path type %s is not supported" % listed.strip("/"), file=sys.stderr)
        sys.exit(1)
    trash_path = pathlib.Path(TRASH_JSON)
    # A listing a test provides for one folder: `/my-files` reads my-files.json.
    section_listing = pathlib.Path(HERE, listed.strip("/").replace("/", "_") + ".json")
    if listed.rstrip("/") in ("/trash", "/photos-trash") and trash_path.exists():
        sys.stdout.write(trash_path.read_text())
    elif listed.strip("/") and section_listing.exists():
        sys.stdout.write(section_listing.read_text())
    else:
        print("[]")
    sys.exit(0)
# A rename to a name starting with `fail` is refused, for the tests of a
# two-step move whose second step fails.
if verb == "filesystem" and sub == "rename" and args[-1].startswith("fail"):
    print("Error: the name was rejected", file=sys.stderr)
    sys.exit(1)
if verb == "filesystem" and sub in (
    "upload", "trash", "delete", "rename", "create-folder", "copy", "move"
):
    sys.exit(0)
if verb == "sharing" and sub == "set-url":
    response = pathlib.Path(HERE, "share-response.json")
    if response.exists():
        sys.stdout.write(response.read_text())
    else:
        print(json.dumps({"url": "https://drive.proton.me/urls/SHIM#key"}))
    sys.exit(0)
print("unhandled", args, file=sys.stderr)
sys.exit(1)
