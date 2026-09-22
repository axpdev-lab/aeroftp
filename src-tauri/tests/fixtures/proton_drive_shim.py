#!/usr/bin/env python3
# Stand-in for the `proton-drive` CLI, used by `cli_sequence_tests` in
# src/providers/proton.rs. It records argv and fakes just enough behaviour for
# the provider sequences under test. It is a checked-in executable on purpose:
# see `link_shim` in that module for why the tests must not write it themselves.
import json, os, pathlib, sys

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
if verb == "filesystem" and sub == "download":
    dest = pathlib.Path(args[-1])
    remote = args[-2]
    name = pathlib.Path(remote).name
    dest.mkdir(parents=True, exist_ok=True)
    target = dest / name
    if "-f" in args:
        i = args.index("-f")
        strat = args[i + 1] if i + 1 < len(args) else ""
        if strat == "remove" and target.exists():
            target.unlink()
    target.write_text("REMOTE-CONTENT")
    sys.exit(0)
if verb == "filesystem" and sub == "info":
    path = next((a for a in args[2:] if not a.startswith("-")), "/x")
    name = pathlib.Path(path).name
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
if verb == "filesystem" and sub in (
    "upload", "trash", "delete", "rename", "create-folder", "copy", "move"
):
    sys.exit(0)
print("unhandled", args, file=sys.stderr)
sys.exit(1)
