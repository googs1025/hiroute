#!/usr/bin/env python3
"""Create or verify a deterministic HiRoute standalone user-install archive."""

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import platform
import re
import tarfile

REPO = Path(__file__).resolve().parent.parent
SCHEMA = "hiroute.standalone-package/v1"
TARGETS = {
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "aarch64-apple-darwin",
}
PORTABLE = re.compile(r"^[A-Za-z0-9._-]+$")
HEX_SHA256 = re.compile(r"^[a-fA-F0-9]{64}$")
MAX_ARCHIVE = 1024 * 1024 * 1024
NOTICE_FILES = {
    "THIRD-PARTY-LICENSES.txt",
    "third-party-licenses.json",
}
EXPECTED_FILES = {
    "bin/hiroute",
    "bin/hirouted",
    "libexec/cliproxyapi",
    "licenses/CLIProxyAPI-LICENSE",
    "licenses/HiRoute-LICENSE",
    "licenses/THIRD-PARTY-LICENSES.txt",
    "licenses/third-party-licenses.json",
    "skills/hiroute-management/SKILL.md",
    "docs/standalone-cli.md",
}


def load_json_bytes(value):
    def unique(pairs):
        result = {}
        for key, item in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key: {key}")
            result[key] = item
        return result

    return json.loads(value, object_pairs_hook=unique)


def digest(path):
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(chunk)
    return value.hexdigest()


def host_target():
    machine = {"AMD64": "x86_64", "arm64": "aarch64"}.get(
        platform.machine(), platform.machine()
    )
    system = {"Linux": "unknown-linux-gnu", "Darwin": "apple-darwin"}.get(
        platform.system()
    )
    if not system:
        raise ValueError("standalone packaging supports only Linux and macOS")
    return f"{machine}-{system}"


def checked_source(path, executable=False):
    if path.is_symlink():
        raise ValueError(f"package source is not a regular file: {path}")
    path = path.resolve(strict=True)
    if not path.is_file():
        raise ValueError(f"package source is not a regular file: {path}")
    mode = path.stat().st_mode
    if mode & 0o022:
        raise ValueError(f"package source is group/world writable: {path}")
    if executable and not mode & 0o111:
        raise ValueError(f"package source is not executable: {path}")
    return path


def validate_notices(directory):
    if directory.is_symlink() or not directory.is_dir():
        raise ValueError("third-party notices must be a directory")
    actual = {path.name for path in directory.iterdir() if path.is_file()}
    if (
        actual != NOTICE_FILES
        or any(path.is_symlink() or not path.is_file() for path in directory.iterdir())
    ):
        raise ValueError("third-party notice inventory is incomplete")
    manifest = load_json_bytes((directory / "third-party-licenses.json").read_bytes())
    if (
        not isinstance(manifest, dict)
        or set(manifest) != {"schema", "inputs", "packages", "documents"}
        or manifest.get("schema") != "hiroute.third-party-licenses/v1"
        or not isinstance(manifest.get("inputs"), dict)
        or not isinstance(manifest.get("packages"), list)
        or not manifest["packages"]
        or not isinstance(manifest.get("documents"), list)
        or not manifest["documents"]
        or not (directory / "THIRD-PARTY-LICENSES.txt").read_text().strip()
    ):
        raise ValueError("third-party notice manifest is invalid")
    return directory


def payload(args, notices):
    skill = checked_source(REPO / "assets/skills/hiroute-management/SKILL.md")
    files = {
        "bin/hiroute": checked_source(args.hiroute, True),
        "bin/hirouted": checked_source(args.hirouted, True),
        "libexec/cliproxyapi": checked_source(args.cpa_binary, True),
        "licenses/CLIProxyAPI-LICENSE": checked_source(args.cpa_license),
        "licenses/HiRoute-LICENSE": checked_source(REPO / "LICENSE"),
        "skills/hiroute-management/SKILL.md": skill,
        "docs/standalone-cli.md": checked_source(REPO / "docs/standalone-cli.md"),
    }
    for name in sorted(NOTICE_FILES):
        files[f"licenses/{name}"] = checked_source(notices / name)
    return files


def deterministic_archive(destination, files):
    destination.parent.mkdir(parents=True, exist_ok=True)
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for relative, source in sorted(files.items()):
            data = source.read_bytes()
            info = tarfile.TarInfo(relative)
            info.size = len(data)
            info.mode = 0o755 if relative.startswith(("bin/", "libexec/")) else 0o644
            info.mtime = 0
            info.uid = info.gid = 0
            info.uname = info.gname = ""
            archive.addfile(info, io.BytesIO(data))
    with destination.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            compressed.write(buffer.getvalue())


def build(args):
    if args.target not in TARGETS:
        raise ValueError("unsupported standalone target")
    if (
        not PORTABLE.fullmatch(args.version)
        or len(args.version) > 64
        or not PORTABLE.fullmatch(args.revision)
        or len(args.revision) > 128
        or not args.cpa_version
        or len(args.cpa_version) > 128
        or any(ord(value) < 0x20 for value in args.cpa_version)
    ):
        raise ValueError("version and revision must be portable identifiers")
    files = payload(args, validate_notices(args.notices.resolve()))
    name = f"hiroute-{args.version}-{args.revision[:12]}-{args.target}.tar.gz"
    archive = args.output.resolve() / name
    deterministic_archive(archive, files)
    manifest = {
        "schema": SCHEMA,
        "version": args.version,
        "revision": args.revision,
        "target": args.target,
        "archive": {
            "filename": archive.name,
            "sha256": digest(archive),
            "size": archive.stat().st_size,
        },
        "files": {
            relative: {
                "sha256": digest(source),
                "size": source.stat().st_size,
                "mode": "executable"
                if relative.startswith(("bin/", "libexec/"))
                else "read_only",
            }
            for relative, source in sorted(files.items())
        },
        "cpa": {
            "version": args.cpa_version,
            "sha256": digest(files["libexec/cliproxyapi"]),
        },
        "distribution": "integration-candidate",
    }
    manifest_path = archive.with_suffix(archive.suffix + ".json")
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n")
    verify(manifest_path, archive)
    print(json.dumps({"archive": str(archive), "manifest": str(manifest_path)}, sort_keys=True))


def safe_members(archive, expected):
    result = {}
    with tarfile.open(archive, "r:gz") as bundle:
        for member in bundle:
            if len(result) >= len(expected):
                raise ValueError("archive contains an unsafe or duplicate entry")
            path = Path(member.name)
            if (
                not member.isfile()
                or path.is_absolute()
                or ".." in path.parts
                or member.name in result
                or member.name not in expected
                or member.size != expected[member.name]["size"]
            ):
                raise ValueError("archive contains an unsafe or duplicate entry")
            stream = bundle.extractfile(member)
            if stream is None:
                raise ValueError("archive entry is unreadable")
            data = stream.read(member.size + 1)
            if len(data) != member.size:
                raise ValueError("archive entry differs from its bounded manifest size")
            result[member.name] = (data, member.mode & 0o777)
    return result


def verify(manifest_path, archive_path):
    manifest = load_json_bytes(Path(manifest_path).read_bytes())
    archive = Path(archive_path)
    expected_files = EXPECTED_FILES
    if (
        not isinstance(manifest, dict)
        or set(manifest)
        != {"schema", "version", "revision", "target", "archive", "files", "cpa", "distribution"}
        or manifest.get("schema") != SCHEMA
        or manifest.get("target") not in TARGETS
        or not isinstance(manifest.get("version"), str)
        or not PORTABLE.fullmatch(manifest.get("version", ""))
        or len(manifest["version"]) > 64
        or not isinstance(manifest.get("revision"), str)
        or not PORTABLE.fullmatch(manifest.get("revision", ""))
        or len(manifest["revision"]) > 128
        or manifest.get("distribution") != "integration-candidate"
        or not isinstance(manifest.get("files"), dict)
        or set(manifest["files"]) != expected_files
        or not isinstance(manifest.get("archive"), dict)
        or set(manifest["archive"]) != {"filename", "sha256", "size"}
        or not isinstance(manifest.get("cpa"), dict)
        or set(manifest["cpa"]) != {"version", "sha256"}
    ):
        raise ValueError("standalone manifest is invalid")
    expected_archive = manifest.get("archive", {})
    if (
        not isinstance(expected_archive.get("filename"), str)
        or not expected_archive["filename"]
        or len(expected_archive["filename"]) > 240
        or expected_archive["filename"] in {".", ".."}
        or Path(expected_archive["filename"]).name != expected_archive["filename"]
        or not expected_archive["filename"].endswith(".tar.gz")
        or archive.name != expected_archive.get("filename")
        or not isinstance(expected_archive.get("sha256"), str)
        or not HEX_SHA256.fullmatch(expected_archive["sha256"])
        or type(expected_archive.get("size")) is not int
        or not 0 < expected_archive["size"] <= MAX_ARCHIVE
        or archive.stat().st_size != expected_archive.get("size")
        or digest(archive) != expected_archive.get("sha256")
    ):
        raise ValueError("standalone archive digest differs from manifest")
    total = 0
    for name, facts in manifest["files"].items():
        expected_mode = "executable" if name.startswith(("bin/", "libexec/")) else "read_only"
        if (
            not isinstance(facts, dict)
            or set(facts) != {"sha256", "size", "mode"}
            or not isinstance(facts.get("sha256"), str)
            or not HEX_SHA256.fullmatch(facts["sha256"])
            or type(facts.get("size")) is not int
            or facts["size"] < 0
            or facts.get("mode") != expected_mode
        ):
            raise ValueError(f"standalone manifest entry is invalid: {name}")
        total += facts["size"]
    if total > MAX_ARCHIVE:
        raise ValueError("standalone manifest payload is too large")
    members = safe_members(archive, manifest["files"])
    if set(members) != set(manifest["files"]):
        raise ValueError("standalone archive inventory differs from manifest")
    for name, facts in manifest["files"].items():
        expected_mode = "executable" if name.startswith(("bin/", "libexec/")) else "read_only"
        data, mode = members[name]
        if (
            hashlib.sha256(data).hexdigest() != facts.get("sha256")
            or len(data) != facts.get("size")
            or mode != (0o755 if expected_mode == "executable" else 0o644)
        ):
            raise ValueError(f"standalone archive entry differs from manifest: {name}")
    if (
        not isinstance(manifest["cpa"].get("version"), str)
        or not manifest["cpa"]["version"]
        or len(manifest["cpa"]["version"]) > 128
        or any(ord(value) < 0x20 for value in manifest["cpa"]["version"])
        or not isinstance(manifest["cpa"].get("sha256"), str)
        or not HEX_SHA256.fullmatch(manifest["cpa"]["sha256"])
        or manifest["cpa"]["sha256"] != manifest["files"]["libexec/cliproxyapi"]["sha256"]
    ):
        raise ValueError("CPA identity differs from archive inventory")
    return manifest


def parser():
    result = argparse.ArgumentParser()
    sub = result.add_subparsers(dest="command", required=True)
    create = sub.add_parser("build")
    create.add_argument("--version", required=True)
    create.add_argument("--revision", required=True)
    create.add_argument("--target", default=host_target())
    create.add_argument("--hiroute", type=Path, required=True)
    create.add_argument("--hirouted", type=Path, required=True)
    create.add_argument("--cpa-binary", type=Path, required=True)
    create.add_argument("--cpa-version", required=True)
    create.add_argument("--cpa-license", type=Path, required=True)
    create.add_argument("--notices", type=Path, required=True)
    create.add_argument("--output", type=Path, required=True)
    check = sub.add_parser("verify")
    check.add_argument("manifest", type=Path)
    check.add_argument("archive", type=Path)
    return result


def main():
    args = parser().parse_args()
    if args.command == "build":
        build(args)
    else:
        manifest = verify(args.manifest, args.archive)
        print(json.dumps({"version": manifest["version"], "target": manifest["target"]}))


if __name__ == "__main__":
    main()
