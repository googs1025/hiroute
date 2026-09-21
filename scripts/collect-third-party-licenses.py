#!/usr/bin/env python3
"""Collect deterministic license material for HiRoute distribution packages.

The collector uses the locked Rust/npm dependency trees and the exact pinned
CLIProxyAPI source commit.  It intentionally emits no timestamps or local
paths, so the result can be compared across packaging hosts.
"""

from __future__ import annotations

import argparse
from collections import defaultdict
import hashlib
import io
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile


REPO = Path(__file__).resolve().parent.parent
SCHEMA = "hiroute.third-party-licenses/v1"
LICENSE_NAMES = re.compile(
    r"(?:^|[-_.])(licen[cs]e|copying|notice|copyright|unlicense|patents?)(?:$|[-_.])",
    re.IGNORECASE,
)
MAX_DOCUMENT_BYTES = 4 * 1024 * 1024

MIT_BODY = """MIT License

Copyright (c) {owner}

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the \"Software\"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"""

BSD3_BODY = """BSD 3-Clause License

Copyright (c) {owner}
All rights reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice,
   this list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS \"AS IS\"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE
ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE
LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR
CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF
SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS
INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN
CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE)
ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE
POSSIBILITY OF SUCH DAMAGE.
"""


def run(*args: str, cwd: Path = REPO, env: dict[str, str] | None = None) -> str:
    return subprocess.run(
        args,
        cwd=cwd,
        env=env,
        check=True,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def license_files(directory: Path) -> list[Path]:
    if not directory.is_dir():
        raise ValueError(f"dependency source directory is missing: {directory}")
    result = []
    for path in directory.iterdir():
        if path.is_symlink():
            continue
        if path.is_file() and LICENSE_NAMES.search(path.name):
            if not 0 < path.stat().st_size <= MAX_DOCUMENT_BYTES:
                raise ValueError(f"license document has an invalid size: {path}")
            result.append(path)
    return sorted(result, key=lambda item: item.name.casefold())


def author_text(value: object, fallback: str) -> str:
    if isinstance(value, list):
        values = [str(item).strip() for item in value if str(item).strip()]
        return ", ".join(values) if values else fallback
    if isinstance(value, dict):
        name = str(value.get("name", "")).strip()
        email = str(value.get("email", "")).strip()
        return f"{name} <{email}>" if name and email else name or email or fallback
    if isinstance(value, str) and value.strip():
        return value.strip()
    return fallback


def fallback_document(expression: str, owner: str, apache_text: bytes) -> tuple[str, bytes]:
    normalized = expression.replace("/", " OR ")
    if "Apache-2.0" in normalized:
        return "LICENSE-Apache-2.0", apache_text
    if expression == "MIT":
        return "LICENSE-MIT", MIT_BODY.format(owner=owner).encode()
    if expression == "BSD-3-Clause":
        return "LICENSE-BSD-3-Clause", BSD3_BODY.format(owner=owner).encode()
    raise ValueError(
        f"dependency declares {expression!r} but publishes no license document; "
        "add a reviewed fallback before distribution"
    )


def document_bytes(path: Path) -> bytes:
    value = path.read_bytes()
    value.decode("utf-8")
    return value


def package_record(
    ecosystem: str,
    name: str,
    version: str,
    declared_license: str,
    source: str | None,
    documents: list[tuple[str, bytes]],
) -> dict[str, object]:
    if not documents:
        raise ValueError(f"no license documents collected for {ecosystem}:{name}@{version}")
    return {
        "id": f"{ecosystem}:{name}@{version}",
        "ecosystem": ecosystem,
        "name": name,
        "version": version,
        "declared_license": declared_license,
        "source": source,
        "documents": [
            {"name": filename, "sha256": sha256_bytes(value)}
            for filename, value in documents
        ],
        "_document_bytes": documents,
    }


def collect_cargo(apache_text: bytes, target: str | None) -> tuple[list[dict[str, object]], str]:
    args = ["cargo", "metadata", "--locked", "--format-version", "1"]
    if target:
        args += ["--filter-platform", target]
    metadata = json.loads(run(*args))
    packages = []
    for item in metadata["packages"]:
        if item.get("source") is None:
            continue
        directory = Path(item["manifest_path"]).parent
        documents = [(path.name, document_bytes(path)) for path in license_files(directory)]
        expression = item.get("license") or ""
        if not documents:
            documents = [fallback_document(
                expression,
                author_text(item.get("authors"), item["name"]),
                apache_text,
            )]
        packages.append(package_record(
            "cargo",
            item["name"],
            item["version"],
            expression,
            item.get("source"),
            documents,
        ))
    return packages, sha256_file(REPO / "Cargo.lock")


def collect_npm(root: Path, apache_text: bytes) -> tuple[list[dict[str, object]], str]:
    lock_path = root / "package-lock.json"
    lock = json.loads(lock_path.read_text())
    if lock.get("lockfileVersion") != 3 or not isinstance(lock.get("packages"), dict):
        raise ValueError("npm license collection requires a package-lock v3 package map")
    packages = []
    for relative, locked in sorted(lock["packages"].items()):
        if not relative or locked.get("dev") is True:
            continue
        directory = root / relative
        manifest_path = directory / "package.json"
        if not manifest_path.is_file():
            raise ValueError(f"npm dependency is not installed: {relative}; run npm ci first")
        manifest = json.loads(manifest_path.read_text())
        name = manifest.get("name")
        version = locked.get("version") or manifest.get("version")
        expression = locked.get("license") or manifest.get("license") or ""
        if not isinstance(name, str) or not isinstance(version, str) or not expression:
            raise ValueError(f"npm dependency identity/license is incomplete: {relative}")
        documents = [(path.name, document_bytes(path)) for path in license_files(directory)]
        if not documents:
            documents = [fallback_document(
                expression,
                author_text(manifest.get("author"), name),
                apache_text,
            )]
        packages.append(package_record(
            "npm",
            name,
            version,
            expression,
            manifest.get("repository") if isinstance(manifest.get("repository"), str) else None,
            documents,
        ))
    return packages, sha256_file(lock_path)


def json_sequence(value: str) -> list[dict[str, object]]:
    decoder = json.JSONDecoder()
    result = []
    offset = 0
    while offset < len(value):
        while offset < len(value) and value[offset].isspace():
            offset += 1
        if offset == len(value):
            break
        item, offset = decoder.raw_decode(value, offset)
        if not isinstance(item, dict):
            raise ValueError("go mod download returned a non-object JSON value")
        result.append(item)
    return result


def extract_regular_tree(bundle: tarfile.TarFile, destination: Path) -> None:
    """Extract a source archive without relying on version-specific tar filters."""
    for member in bundle.getmembers():
        relative = Path(member.name)
        if relative.is_absolute() or ".." in relative.parts or not relative.parts:
            raise ValueError("CPA source archive contains an unsafe path")
        target = destination.joinpath(*relative.parts)
        if member.isdir():
            target.mkdir(parents=True, exist_ok=True)
            continue
        if not member.isfile():
            raise ValueError("CPA source archive contains a non-regular entry")
        source = bundle.extractfile(member)
        if source is None:
            raise ValueError("CPA source archive entry is unreadable")
        target.parent.mkdir(parents=True, exist_ok=True)
        with target.open("wb") as output:
            shutil.copyfileobj(source, output)


def extract_cpa(source_repo: Path, destination: Path) -> tuple[dict[str, object], Path]:
    pin_path = REPO / "vendor/cpa/source.json"
    pin = json.loads(pin_path.read_text())
    patch = pin_path.parent / pin["patch"]
    if sha256_file(patch) != pin["patch_sha256"]:
        raise ValueError("CPA source patch checksum mismatch")
    archive = subprocess.check_output([
        "git", "-C", str(source_repo), "archive", pin["commit"]
    ])
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:") as bundle:
        extract_regular_tree(bundle, destination)
    run("git", "apply", str(patch), cwd=destination)
    return pin, pin_path


def collect_go(source_repo: Path) -> tuple[list[dict[str, object]], dict[str, str]]:
    with tempfile.TemporaryDirectory(prefix="hiroute-cpa-license-") as temporary:
        source = Path(temporary)
        pin, pin_path = extract_cpa(source_repo, source)
        root_license = source / "LICENSE"
        if not root_license.is_file():
            raise ValueError("pinned CPA source has no LICENSE")
        module_line = (source / "go.mod").read_text().splitlines()[0]
        if not module_line.startswith("module ") or not module_line.removeprefix("module ").strip():
            raise ValueError("pinned CPA source has an invalid module path")
        module_name = module_line.removeprefix("module ").strip()
        packages = [package_record(
            "go",
            module_name,
            pin["version"],
            "MIT",
            "https://github.com/router-for-me/CLIProxyAPI",
            [(root_license.name, document_bytes(root_license))],
        )]
        env = os.environ.copy()
        env["GOWORK"] = "off"
        modules = json_sequence(run("go", "mod", "download", "-json", "all", cwd=source, env=env))
        for item in modules:
            if item.get("Error"):
                raise ValueError(f"cannot download Go module {item.get('Path')}: {item['Error']}")
            directory = item.get("Dir")
            path = item.get("Path")
            version = item.get("Version")
            if not isinstance(directory, str) or not isinstance(path, str) or not isinstance(version, str):
                raise ValueError("Go module download did not return path/version/source directory")
            license_paths = license_files(Path(directory))
            documents = [(candidate.name, document_bytes(candidate)) for candidate in license_paths]
            expression = "license-file"
            if not documents and path == "github.com/mattn/go-localereader":
                expression = "MIT"
                documents = [("LICENSE-MIT", MIT_BODY.format(
                    owner="Yasuhiro Matsumoto (a.k.a. mattn)"
                ).encode())]
            if not documents:
                raise ValueError(f"Go module publishes no license document: {path}@{version}")
            origin = item.get("Origin")
            origin_url = origin.get("URL") if isinstance(origin, dict) else None
            packages.append(package_record(
                "go", path, version, expression, origin_url, documents
            ))
        inputs = {
            "commit": pin["commit"],
            "go_sum_sha256": sha256_file(source / "go.sum"),
            "pin_sha256": sha256_file(pin_path),
        }
        return packages, inputs


def render(packages: list[dict[str, object]], inputs: dict[str, object], output: Path) -> None:
    packages.sort(key=lambda item: item["id"])
    documents: dict[str, bytes] = {}
    document_packages: dict[str, set[str]] = defaultdict(set)
    document_names: dict[str, set[str]] = defaultdict(set)
    for package in packages:
        for name, value in package.pop("_document_bytes"):
            digest = sha256_bytes(value)
            existing = documents.setdefault(digest, value)
            if existing != value:
                raise ValueError("SHA-256 collision while collecting license documents")
            document_packages[digest].add(str(package["id"]))
            document_names[digest].add(name)

    manifest_documents = []
    sections = [
        "HiRoute third-party licenses and notices\n",
        "Generated deterministically from locked dependency sources. "
        "Package identifiers using the same document are grouped together.\n",
    ]
    for digest in sorted(documents):
        value = documents[digest]
        names = sorted(document_names[digest])
        owners = sorted(document_packages[digest])
        manifest_documents.append({
            "sha256": digest,
            "source_names": names,
            "packages": owners,
        })
        sections.extend([
            "\n" + "=" * 78 + "\n",
            f"Document SHA256: {digest}\n",
            f"Source names: {', '.join(names)}\n",
            "Packages:\n",
            "".join(f"  - {owner}\n" for owner in owners),
            "-" * 78 + "\n",
            value.decode("utf-8"),
            "\n" if not value.endswith(b"\n") else "",
        ])

    manifest = {
        "schema": SCHEMA,
        "inputs": inputs,
        "packages": packages,
        "documents": manifest_documents,
    }
    if output.exists():
        if not output.is_dir() or any(output.iterdir()):
            raise ValueError("license render directory must be empty")
    else:
        output.mkdir(parents=True)
    manifest_path = output / "third-party-licenses.json"
    manifest_path.write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    )
    text_path = output / "THIRD-PARTY-LICENSES.txt"
    text_path.write_text("".join(sections))
    manifest_path.chmod(0o644)
    text_path.chmod(0o644)


def collect(args: argparse.Namespace) -> dict[str, object]:
    output = args.output.resolve()
    if output.exists():
        raise ValueError("output directory already exists")
    output.parent.mkdir(parents=True, exist_ok=True)
    apache_text = (REPO / "LICENSE").read_bytes()
    apache_text.decode("utf-8")
    packages, cargo_digest = collect_cargo(apache_text, args.cargo_target)
    inputs: dict[str, object] = {"cargo_lock_sha256": cargo_digest}
    if args.npm_root:
        npm_packages, npm_digest = collect_npm(args.npm_root.resolve(), apache_text)
        packages.extend(npm_packages)
        inputs["npm_lock_sha256"] = npm_digest
    go_packages, go_inputs = collect_go(args.cpa_source_repo.resolve())
    packages.extend(go_packages)
    inputs["cpa"] = go_inputs
    staging = Path(tempfile.mkdtemp(prefix=".hiroute-licenses-", dir=output.parent))
    try:
        render(packages, inputs, staging)
        staging.replace(output)
    except Exception:
        shutil.rmtree(staging, ignore_errors=True)
        raise
    return {
        "output": str(output),
        "packages": len(packages),
        "documents": len(json.loads((output / "third-party-licenses.json").read_text())["documents"]),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--cpa-source-repo", required=True, type=Path)
    parser.add_argument("--cargo-target")
    parser.add_argument("--npm-root", type=Path)
    args = parser.parse_args()
    try:
        print(json.dumps(collect(args), sort_keys=True))
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
        print(str(error), file=os.sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
