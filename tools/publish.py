# SPDX-License-Identifier: GPL-3.0-or-later
"""Publish Open3D app packages, the signed launcher catalog and the launcher itself.

    python -I tools\\publish.py app opentools [--version 0.5.1] [--dry-run]
        Builds `<id>-<ver>-windows-x64-3dsmax.zip` from the app's `vX.Y.Z` tag (`git archive`, never the
        working tree): runs the app's build steps (tools/apps.json), checks the bundle's AppVersion, adds the
        app's LICENSE as `<bundle>/LICENSE`, and attaches the zip and `SHA256SUMS.txt` to the app's own GitHub
        release `vX.Y.Z` (created from that version's CHANGELOG section when it doesn't exist yet). The
        launcher reads each app's versions from those releases.
    python -I tools\\publish.py catalog [--dry-run] [--no-push]
        Signs `crates/core/catalog.toml` as `launcher-catalog.toml` + `.sig` (Ed25519, the key in
        `%USERPROFILE%\\.open3d\\launcher-catalog-signing-key.pem`, never in git), publishes the app icons in
        `launcher-app-icons/`, commits and pushes. Needed only when the list of apps changes.
    python -I tools\\publish.py launcher [--dry-run]
        Builds the launcher's MSI and portable zip with `packaging/windows/package.ps1` (version from
        Cargo.toml; needs WiX 5 on PATH), writes `SHA256SUMS.txt`, tags the committed HEAD `vX.Y.Z`, pushes the
        tag and creates the GitHub release with that version's CHANGELOG section. Installed launchers update
        themselves from it.

Run it with 3ds Max's Python (an app's own build step may need PySide6).
"""

import argparse
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
DIST = LAUNCHER / "dist"
CATALOG_SOURCE = LAUNCHER / "crates" / "core" / "catalog.toml"
CATALOG = LAUNCHER / "launcher-catalog.toml"
ICONS = LAUNCHER / "launcher-app-icons"
APP_ICONS = LAUNCHER / "assets" / "app-icons"
PUBLIC_KEY = LAUNCHER / "crates" / "core" / "assets" / "launcher-catalog-public-key.pem"
SIGNING_KEY = Path(os.path.expandvars(r"%USERPROFILE%\.open3d\launcher-catalog-signing-key.pem"))
VERSION_TAG = re.compile(r"^v(\d+(?:\.\d+)*)$")


class PublishError(Exception):
    pass


def run(cmd, cwd=None, binary=False):
    result = subprocess.run(cmd, cwd=cwd, capture_output=True, text=not binary)
    if result.returncode != 0:
        err = result.stderr if not binary else result.stderr.decode("utf-8", "replace")
        raise PublishError("Command failed: %s\n%s" % (" ".join(map(str, cmd)), (err or "").strip()))
    return result.stdout


def find_tool(local, name):
    path = os.path.expandvars(local)
    if os.path.isfile(path):
        return path
    found = shutil.which(name)
    if not found:
        raise PublishError("%s not found" % name)
    return found


def gh():
    return find_tool(r"%LOCALAPPDATA%\Programs\GitHubCLI\bin\gh.exe", "gh")


def openssl():
    return find_tool(r"%ProgramFiles%\Git\usr\bin\openssl.exe", "openssl")


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
    data = run(["git", "-C", str(checkout), "archive", "--format=tar", tag], binary=True)
    with tarfile.open(fileobj=io.BytesIO(data)) as tar:
        tar.extractall(dest, filter="data")


def bundle_version(bundle_dir):
    xml = (bundle_dir / "PackageContents.xml").read_text(encoding="utf-8")
    m = re.search(r'\sAppVersion\s*=\s*"([^"]+)"', xml)
    if not m:
        raise PublishError("No AppVersion in %s" % bundle_dir)
    return m.group(1)


def changelog_section(path, version):
    """The body of the '## <version> ...' section, without its heading."""
    out, inside = [], False
    head = re.compile(r"^## %s\b" % re.escape(version))
    for line in path.read_text(encoding="utf-8").splitlines():
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


def zip_bundle(bundle_dir, bundle_name, zip_path, license_file):
    """The bundle folder at the zip's root, with the app's LICENSE inside it (the GPL text travels with it)."""
    files = sorted(p for p in bundle_dir.rglob("*") if p.is_file() and "__pycache__" not in p.parts and p.suffix != ".pyc")
    if zip_path.exists():
        zip_path.unlink()
    with zipfile.ZipFile(zip_path, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        for f in files:
            z.write(f, bundle_name + "/" + f.relative_to(bundle_dir).as_posix())
        if not (bundle_dir / "LICENSE").exists():
            z.write(license_file, bundle_name + "/LICENSE")
    return len(files)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def publish_app(app_id, version=None, dry_run=False):
    apps = json.loads(APPS_FILE.read_text(encoding="utf-8"))
    if app_id not in apps:
        raise PublishError("Unknown app %r (known: %s)" % (app_id, ", ".join(apps)))
    app = apps[app_id]
    checkout = (LAUNCHER / app["checkout"]).resolve()
    version, tag = pick_tag(checkout, version)
    asset = "%s-%s-windows-x64-3dsmax.zip" % (app_id, version)
    print("%s %s (tag %s)" % (app["name"], version, tag))
    DIST.mkdir(exist_ok=True)
    zip_path = DIST / asset
    work = Path(tempfile.mkdtemp(prefix="open3d-publish-"))
    try:
        export_tag(checkout, tag, work)
        for script in app.get("build", []):
            print("  build: %s" % script)
            run([sys.executable, "-I", str(work / script)], cwd=work)
        bundle_dir = work / app["bundle_path"]
        if not (bundle_dir / "PackageContents.xml").is_file():
            raise PublishError("No %s at tag %s" % (app["bundle_path"], tag))
        found = bundle_version(bundle_dir)
        if version_key(found) != version_key(version):
            raise PublishError("Tag %s but the bundle says AppVersion %s" % (tag, found))
        if not (work / "LICENSE").is_file():
            raise PublishError("No LICENSE at tag %s: every Open3D app ships its license" % tag)
        notes = changelog_section(work / app["changelog"], version)
        count = zip_bundle(bundle_dir, app["bundle"], zip_path, work / "LICENSE")
    finally:
        shutil.rmtree(work, ignore_errors=True)
    digest = sha256(zip_path)
    sums = DIST / ("SHA256SUMS-%s-%s.txt" % (app_id, version))
    sums.write_text("%s  %s\n" % (digest, asset), encoding="utf-8", newline="\n")
    print("  %s: %d files, %.0f KB, sha256 %s" % (asset, count, zip_path.stat().st_size / 1024, digest))
    if dry_run:
        print("Dry run: nothing uploaded.")
        return

    repo = app["repo"]
    exists = subprocess.run([gh(), "release", "view", tag, "--repo", repo], capture_output=True).returncode == 0
    upload_dir = Path(tempfile.mkdtemp(prefix="open3d-upload-"))
    try:
        named_sums = upload_dir / "SHA256SUMS.txt"
        shutil.copyfile(sums, named_sums)
        if not exists:
            notes_file = upload_dir / "notes.md"
            notes_file.write_text(notes + "\n", encoding="utf-8")
            run([gh(), "release", "create", tag, "--repo", repo, "--verify-tag", "--title", "%s %s" % (app["name"], version),
                 "--notes-file", str(notes_file)])
            print("  release %s created in %s" % (tag, repo))
        run([gh(), "release", "upload", tag, str(zip_path), str(named_sums), "--repo", repo, "--clobber"])
    finally:
        shutil.rmtree(upload_dir, ignore_errors=True)
    print("  %s and SHA256SUMS.txt attached to %s %s" % (asset, repo, tag))


def sign(path):
    """`<path>.sig`: the base64 Ed25519 signature of the file's exact bytes."""
    if not SIGNING_KEY.is_file():
        raise PublishError("No signing key at %s" % SIGNING_KEY)
    # Git for Windows' openssl can't open UNC paths (the repo is on a NAS): sign a local copy of the bytes.
    with tempfile.TemporaryDirectory() as tmp:
        local, pub, raw = Path(tmp) / "data", Path(tmp) / "public.pem", Path(tmp) / "sig.bin"
        shutil.copyfile(path, local)
        shutil.copyfile(PUBLIC_KEY, pub)
        run([openssl(), "pkeyutl", "-sign", "-inkey", str(SIGNING_KEY), "-rawin", "-in", str(local), "-out", str(raw)])
        run([openssl(), "pkeyutl", "-verify", "-pubin", "-inkey", str(pub), "-rawin", "-in", str(local), "-sigfile", str(raw)])
        sig = __import__("base64").b64encode(raw.read_bytes()).decode("ascii")
    Path(str(path) + ".sig").write_bytes((sig + "\n").encode("ascii"))


def publish_catalog(dry_run=False, push=True):
    data = CATALOG_SOURCE.read_bytes()
    if b"\r" in data:
        raise PublishError("%s has CR line ends; the signature covers LF bytes" % CATALOG_SOURCE)
    CATALOG.write_bytes(data)
    sign(CATALOG)
    ICONS.mkdir(exist_ok=True)
    for m in re.finditer(rb'(?m)^id = "([a-z0-9-]+)"', data):
        app_id = m.group(1).decode()
        icon = APP_ICONS / ("%s.png" % app_id)
        digest = sha256(icon)
        shutil.copyfile(icon, ICONS / ("%s-%s.png" % (app_id, digest)))
        if ('icon_sha256 = "%s"' % digest).encode() not in data:
            raise PublishError("%s's icon_sha256 in catalog.toml isn't %s" % (app_id, digest))
    print("launcher-catalog.toml signed and verified; icons in %s" % ICONS.name)
    if dry_run:
        return
    git = ["git", "-C", str(LAUNCHER)]
    run(git + ["add", CATALOG.name, CATALOG.name + ".sig", ICONS.name])
    if not run(git + ["diff", "--cached", "--name-only"]).strip():
        print("  nothing changed")
        return
    run(git + ["commit", "-m", "Catalog: sign launcher-catalog.toml"])
    if push:
        run(git + ["push"])
        print("  pushed")


def workspace_version():
    inside = False
    for line in (LAUNCHER / "Cargo.toml").read_text(encoding="utf-8").splitlines():
        if line.startswith("["):
            inside = line.strip() == "[workspace.package]"
        elif inside and (m := re.match(r'\s*version\s*=\s*"([^"]+)"', line)):
            return m.group(1)
    raise PublishError("No [workspace.package] version in Cargo.toml")


def cargo_target_dir():
    """Where cargo builds, as cargo itself resolves it (CARGO_TARGET_DIR, any `.cargo/config.toml` up the tree)."""
    meta = json.loads(run(["cargo", "metadata", "--format-version", "1", "--no-deps"], cwd=LAUNCHER))
    return meta["target_directory"]


def publish_launcher(dry_run=False):
    version = workspace_version()
    tag = "v%s" % version
    git = ["git", "-C", str(LAUNCHER)]
    if run(git + ["status", "--porcelain", "--untracked-files=no"]).strip():
        raise PublishError("Commit the launcher's changes first: the release is built from the committed HEAD")
    notes = changelog_section(LAUNCHER / "CHANGELOG.md", version)
    out = DIST / "release" / version
    env = dict(os.environ, DIST=str(out), CARGO_TARGET_DIR=cargo_target_dir())
    print("Open3D Launcher %s: building the MSI and the portable zip" % version)
    result = subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File",
                             str(LAUNCHER / "packaging" / "windows" / "package.ps1"), "-Arch", "x64"], env=env)
    if result.returncode != 0:
        raise PublishError("package.ps1 failed")
    assets = [out / ("open3d-launcher-%s-windows-x64.msi" % version), out / ("open3d-launcher-%s-windows-x64-portable.zip" % version)]
    for a in assets:
        if not a.is_file():
            raise PublishError("package.ps1 made no %s" % a.name)
    sums = out / "SHA256SUMS.txt"
    sums.write_text("".join("%s  %s\n" % (sha256(a), a.name) for a in assets), encoding="utf-8", newline="\n")
    for a in assets:
        print("  %s: %.1f MB" % (a.name, a.stat().st_size / 1e6))
    if dry_run:
        print("Dry run: nothing tagged or uploaded (files in %s)." % out)
        return
    head = run(git + ["rev-parse", "HEAD"]).strip()
    tags = run(git + ["tag", "--list", tag]).split()
    if tags:
        if run(git + ["rev-list", "-n", "1", tag]).strip() != head:
            raise PublishError("Tag %s exists and isn't HEAD" % tag)
    else:
        run(git + ["tag", "-a", tag, "-m", "Open3D Launcher %s" % version])
    run(git + ["push", "origin", "main", tag])
    notes_file = out / "notes.md"
    notes_file.write_text(notes + "\n", encoding="utf-8")
    run([gh(), "release", "create", tag, "--repo", "carlesllavata/Open3D", "--verify-tag", "--latest",
         "--title", "Open3D Launcher %s" % version, "--notes-file", str(notes_file)] + [str(a) for a in assets] + [str(sums)])
    print("  release %s created in carlesllavata/Open3D" % tag)


def main(argv=None):
    p = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = p.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("app", help="publish an app's package to its own release")
    a.add_argument("app")
    a.add_argument("--version")
    a.add_argument("--dry-run", action="store_true")
    c = sub.add_parser("catalog", help="sign and publish the launcher catalog")
    c.add_argument("--dry-run", action="store_true")
    c.add_argument("--no-push", action="store_true")
    la = sub.add_parser("launcher", help="build and release the launcher itself")
    la.add_argument("--dry-run", action="store_true")
    args = p.parse_args(argv)
    try:
        if args.cmd == "app":
            publish_app(args.app, args.version, args.dry_run)
        elif args.cmd == "launcher":
            publish_launcher(args.dry_run)
        else:
            publish_catalog(args.dry_run, not args.no_push)
    except PublishError as e:
        print("error: %s" % e, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
