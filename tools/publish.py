# SPDX-License-Identifier: GPL-3.0-or-later
"""Publish one version of an Open3D app for the launcher.

Takes the app's checkout at a release tag (never the working tree), builds its bundle if the app needs a
build step, zips the bundle, creates a release in the public repo with that zip, and adds the version to
catalog.json (committed and pushed).

    python -I tools\\publish.py opentools --dry-run      # build the zip and a catalog preview, upload nothing
    python -I tools\\publish.py opentools                 # latest vX.Y.Z tag
    python -I tools\\publish.py openmanager --version 0.3.0

Run it with 3ds Max's Python (an app's own build step may need PySide6). Apps are described in
tools/apps.json; checkout paths there are relative to this repo.
"""

import argparse
import datetime
import hashlib
import io
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path

LAUNCHER = Path(__file__).resolve().parent.parent
APPS_FILE = LAUNCHER / "tools" / "apps.json"
CATALOG = LAUNCHER / "catalog.json"
DIST = LAUNCHER / "dist"
PUBLIC_REPO = "carlesllavata/Open3D"
VERSION_TAG = re.compile(r"^v(\d+(?:\.\d+)*)$")


class PublishError(Exception):
    pass


def run(cmd, cwd=None, capture=True, binary=False):
    result = subprocess.run(cmd, cwd=cwd, capture_output=capture, text=not binary)
    if result.returncode != 0:
        err = result.stderr if not binary else result.stderr.decode("utf-8", "replace")
        raise PublishError("Command failed: %s\n%s" % (" ".join(map(str, cmd)), (err or "").strip()))
    return result.stdout


def find_gh():
    local = os.path.expandvars(r"%LOCALAPPDATA%\Programs\GitHubCLI\bin\gh.exe")
    if os.path.isfile(local):
        return local
    found = shutil.which("gh")
    if not found:
        raise PublishError("GitHub CLI (gh) not found")
    return found


def version_key(v):
    return tuple(int(p) for p in v.split("."))


def pick_tag(checkout, version):
    tags = run(["git", "-C", str(checkout), "tag", "--list"]).split()
    versions = {m.group(1): t for t in tags if (m := VERSION_TAG.match(t))}
    if not versions:
        raise PublishError("%s has no vX.Y.Z tags" % checkout)
    if version:
        if version not in versions:
            raise PublishError("No tag v%s in %s" % (version, checkout))
        return version, versions[version]
    latest = max(versions, key=version_key)
    return latest, versions[latest]


def export_tag(checkout, tag, dest):
    """The files of `tag` (as committed) into `dest`."""
    data = run(["git", "-C", str(checkout), "archive", "--format=tar", tag], binary=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        tar.extractall(dest, filter="data")


def app_version(bundle_dir):
    xml = (bundle_dir / "PackageContents.xml").read_text(encoding="utf-8")
    m = re.search(r'\sAppVersion\s*=\s*"([^"]+)"', xml)
    if not m:
        raise PublishError("No AppVersion in %s" % bundle_dir)
    return m.group(1)


def changelog_section(path, version):
    """The body of the '## <version> ...' section, without its heading."""
    lines = path.read_text(encoding="utf-8").splitlines()
    out, inside = [], False
    head = re.compile(r"^## %s\b" % re.escape(version))
    for line in lines:
        if line.startswith("## "):
            if inside:
                break
            inside = bool(head.match(line))
            continue
        if inside:
            out.append(line)
    text = "\n".join(out).strip()
    if not text:
        raise PublishError("No '## %s' section in %s" % (version, path.name))
    return text


def zip_bundle(bundle_dir, bundle_name, zip_path):
    files = sorted(
        p for p in bundle_dir.rglob("*")
        if p.is_file() and "__pycache__" not in p.parts and p.suffix != ".pyc"
    )
    if zip_path.exists():
        zip_path.unlink()
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for f in files:
            z.write(f, bundle_name + "/" + f.relative_to(bundle_dir).as_posix())
    return len(files)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def load_catalog():
    if CATALOG.exists():
        return json.loads(CATALOG.read_text(encoding="utf-8"))
    return {"schema": 1, "apps": []}


def with_entry(catalog, apps, entry):
    """Catalog with `entry` added or replaced, apps kept in the order of tools/apps.json."""
    by_id = {a["id"]: a for a in catalog.get("apps", [])}
    by_id[entry["id"]] = entry
    order = list(apps)
    ordered = sorted(by_id.values(), key=lambda a: order.index(a["id"]) if a["id"] in order else len(order))
    new = dict(catalog)
    new["apps"] = ordered
    return new


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False) + "\n", encoding="utf-8", newline="\n")


def publish(app_id, version=None, dry_run=False, push=True, repo=PUBLIC_REPO):
    apps = json.loads(APPS_FILE.read_text(encoding="utf-8"))
    if app_id not in apps:
        raise PublishError("Unknown app %r (known: %s)" % (app_id, ", ".join(apps)))
    app = apps[app_id]
    checkout = (LAUNCHER / app["checkout"]).resolve()
    version, tag = pick_tag(checkout, version)
    release_tag = "%s-v%s" % (app_id, version)
    zip_name = "%s-%s.zip" % (app["name"], version)
    print("%s %s (tag %s in %s)" % (app["name"], version, tag, checkout))

    work = Path(tempfile.mkdtemp(prefix="open3d-publish-"))
    try:
        export_tag(checkout, tag, work)
        for script in app.get("build", []):
            print("  build: %s" % script)
            run([sys.executable, "-I", str(work / script)], cwd=work, capture=True)
        bundle_dir = work / app["bundle_path"]
        if not (bundle_dir / "PackageContents.xml").is_file():
            raise PublishError("No %s at tag %s" % (app["bundle_path"], tag))
        found = app_version(bundle_dir)
        if version_key(found) != version_key(version):
            raise PublishError("Tag %s but the bundle says AppVersion %s" % (tag, found))
        notes = changelog_section(work / app["changelog"], version)

        DIST.mkdir(exist_ok=True)
        zip_path = DIST / zip_name
        count = zip_bundle(bundle_dir, app["bundle"], zip_path)
    finally:
        shutil.rmtree(work, ignore_errors=True)

    date = run(["git", "-C", str(checkout), "log", "-1", "--format=%cs", tag]).strip() or str(datetime.date.today())
    entry = {
        "id": app_id,
        "name": app["name"],
        "summary": app.get("summary", ""),
        "bundle": app["bundle"],
        "max_min": app.get("max_min"),
        "latest": {
            "version": version,
            "date": date,
            "url": "https://github.com/%s/releases/download/%s/%s" % (repo, release_tag, zip_name),
            "sha256": sha256(zip_path),
            "size": zip_path.stat().st_size,
            "notes": notes,
        },
    }
    print("  %s: %d files, %.0f KB, sha256 %s" % (zip_name, count, entry["latest"]["size"] / 1024,
                                                 entry["latest"]["sha256"]))
    catalog = with_entry(load_catalog(), apps, entry)

    if dry_run:
        preview = DIST / "catalog.preview.json"
        write_json(preview, catalog)
        print("Dry run: nothing uploaded. Zip: %s  Catalog preview: %s" % (zip_path, preview))
        return

    gh = find_gh()
    exists = subprocess.run([gh, "release", "view", release_tag, "--repo", repo], capture_output=True)
    if exists.returncode == 0:
        raise PublishError("%s is already published in %s" % (release_tag, repo))
    notes_file = DIST / ("%s-notes.md" % release_tag)
    notes_file.write_text(notes + "\n", encoding="utf-8")
    run([gh, "release", "create", release_tag, str(zip_path), "--repo", repo,
         "--title", "%s %s" % (app["name"], version), "--notes-file", str(notes_file)])
    print("  release %s created" % release_tag)

    write_json(CATALOG, catalog)
    run(["git", "-C", str(LAUNCHER), "add", "catalog.json"])
    run(["git", "-C", str(LAUNCHER), "commit", "-m", "Catalog: %s %s" % (app["name"], version)])
    if push:
        run(["git", "-C", str(LAUNCHER), "push"])
        print("  catalog pushed")
    else:
        print("  catalog committed, not pushed (--no-push)")


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    p.add_argument("app", help="app id from tools/apps.json")
    p.add_argument("--version", help="version to publish (default: the latest vX.Y.Z tag)")
    p.add_argument("--dry-run", action="store_true", help="build the zip and a catalog preview only")
    p.add_argument("--no-push", action="store_true", help="commit catalog.json but do not push it")
    p.add_argument("--repo", default=PUBLIC_REPO, help="public repo (default %(default)s)")
    a = p.parse_args(argv)
    try:
        publish(a.app, a.version, a.dry_run, not a.no_push, a.repo)
    except PublishError as e:
        print("error: %s" % e, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
