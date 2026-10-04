"""Generate bundled notices from locked Cargo dependencies and their shipped licenses.

Refresh with: python3 scripts/dependency_notices.py
Check the committed inventory without Cargo: python3 scripts/dependency_notices.py --check
"""

import argparse
import hashlib
from html.parser import HTMLParser
import json
from pathlib import Path
import re
import subprocess
import sys

from install_support import fingerprint


SOURCE = Path(__file__).resolve().parent.parent
FILENAME = "THIRD_PARTY_NOTICES.txt"
TARGETS = ("aarch64-apple-darwin", "x86_64-apple-darwin")
INPUTS = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml")


def input_hashes(source):
    return {name: fingerprint(source / name)["sha256"] for name in INPUTS}


def runtime_packages(metadata):
    """Include build prerequisites conservatively; omit dev-only dependencies."""
    packages = {row["id"]: row for row in metadata["packages"]}
    nodes = {row["id"]: row for row in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    if root not in packages or root not in nodes:
        raise RuntimeError("Cargo did not identify the DeLM dependency root.")
    pending, visited = [root], set()
    while pending:
        identity = pending.pop()
        if identity in visited:
            continue
        visited.add(identity)
        for edge in nodes[identity]["deps"]:
            if any(kind["kind"] in (None, "build") for kind in edge["dep_kinds"]):
                pending.append(edge["pkg"])
    return [packages[identity] for identity in sorted(visited - {root})]


def license_texts(package):
    root = Path(package["manifest_path"]).parent.resolve()
    paths = {path for path in root.rglob("*") if path.is_file()
             and re.match(r"^(licen[sc]e|copying|copyright|notice|unlicense)([._-]|$)", path.name, re.I)}
    declared = package.get("license_file")
    if declared:
        paths.add(root / declared)
    if not package.get("license") and not declared:
        raise RuntimeError(f"{package['name']} {package['version']} has no declared license.")
    if not any(path.name.lower().startswith(("license", "licence", "copying", "copyright", "unlicense"))
               or (declared and path == root / declared) for path in paths):
        raise RuntimeError(f"{package['name']} {package['version']} ships no license text; inspect its provenance before distributing it.")
    result = []
    for path in sorted(paths):
        if not path.resolve().is_relative_to(root) or path.is_symlink():
            raise RuntimeError(f"Refusing a linked or external dependency license: {path}")
        fingerprint(path)
        if path.stat().st_size > 2 * 1024 * 1024:
            raise RuntimeError(f"Dependency license exceeds the notice size limit: {path}")
        text = path.read_text(encoding="utf-8")
        if not text.strip():
            raise RuntimeError(f"Dependency license is empty: {path}")
        result.append((str(path.relative_to(root)), text))
    return result


class LicenseHTML(HTMLParser):
    """Preserve the text and links in rustc's standard-library copyright report."""
    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts = []

    def handle_data(self, data):
        self.parts.append(data)

    def handle_starttag(self, tag, attrs):
        if tag in {"p", "div", "li", "pre", "h1", "h2", "h3", "br"}:
            self.parts.append("\n")
        if tag == "a":
            href = dict(attrs).get("href", "")
            if href.startswith(("https://", "http://")):
                self.parts.append(f"[{href}] ")


def standard_library_notices(source):
    channel = re.search(r'^channel\s*=\s*"([^"]+)"', (source / "rust-toolchain.toml").read_text(), re.M)
    version = subprocess.check_output(["rustc", "--version"], cwd=source, text=True, timeout=15).strip()
    if not channel or not version.startswith("rustc " + channel[1] + " "):
        raise RuntimeError("Generate dependency notices with the repository's pinned Rust toolchain.")
    sysroot = Path(subprocess.check_output(["rustc", "--print", "sysroot"], cwd=source, text=True, timeout=15).strip())
    path = sysroot / "share/doc/rust/COPYRIGHT-library.html"
    fingerprint(path)
    parser = LicenseHTML()
    parser.feed(path.read_text(encoding="utf-8"))
    text = "\n".join(line.rstrip() for line in "".join(parser.parts).splitlines())
    text = re.sub(r"\n{3,}", "\n\n", text).strip()
    if "Rust Standard Library" not in text or "Permission is hereby granted" not in text:
        raise RuntimeError("The pinned toolchain omitted its standard-library license texts.")
    return version, text + "\n"


def render(source, records, standard_library=None):
    inputs = input_hashes(source)
    packages = {}
    for metadata in records:
        for package in runtime_packages(metadata):
            identity = (package["name"], package["version"], package.get("source"))
            if identity in packages and packages[identity] != package:
                raise RuntimeError(f"Cargo returned inconsistent package metadata: {identity}")
            packages[identity] = package
    texts, inventory = {}, []
    for identity, package in sorted(packages.items()):
        entries = []
        for name, content in license_texts(package):
            checksum = hashlib.sha256(content.encode()).hexdigest()
            texts[checksum] = content
            entries.append(f"  {name}: text {checksum}")
        inventory.append("\n".join([
            f"{package['name']} {package['version']}",
            f"  Source: {package.get('source') or 'local workspace dependency'}",
            f"  Declared license: {package.get('license') or package['license_file']}", *entries,
        ]))
    if not inventory:
        raise RuntimeError("Cargo returned no runtime dependencies; refusing an empty notice inventory.")
    if standard_library:
        version, text = standard_library
        checksum = hashlib.sha256(text.encode()).hexdigest()
        texts[checksum] = text
        inventory.append(f"Rust standard library ({version})\n"
                         "  Source: pinned Rust toolchain share/doc/rust/COPYRIGHT-library.html\n"
                         f"  Copyright and license texts: text {checksum}")
    header = ["DeLM runtime third-party notices", "",
              "Generated by: python3 scripts/dependency_notices.py",
              *[f"{name} SHA-256: {checksum}" for name, checksum in inputs.items()], "",
              "This inventory covers the union of the locked Apple Silicon and Intel macOS",
              "dependency graphs, including build prerequisites conservatively, excluding",
              "dev-only dependencies. Their original license choices and notices remain intact.",
              "Identical license texts are included once and referenced by SHA-256.", "",
              "Dependency inventory", "====================", "", "\n\n".join(inventory), "",
              "License texts", "=============", ""]
    for checksum, content in sorted(texts.items()):
        header.extend([f"Text {checksum}", "-" * 72, content.rstrip("\n"), ""])
    return "\n".join(header).rstrip("\n") + "\n"


def generate(source):
    before = input_hashes(source)
    records = []
    for target in TARGETS:
        result = subprocess.run(["cargo", "metadata", "--locked", "--format-version", "1",
                                 "--filter-platform", target, "--manifest-path", str(source / "Cargo.toml")],
                                cwd=source, check=True, text=True, capture_output=True, timeout=120)
        records.append(json.loads(result.stdout))
    content = render(source, records, standard_library_notices(source))
    if input_hashes(source) != before:
        raise RuntimeError("Cargo inputs changed while generating dependency notices.")
    return content


def validate(source):
    path = source / FILENAME
    fingerprint(path)
    content = path.read_text(encoding="utf-8")
    if not all(f"{name} SHA-256: {checksum}\n" in content for name, checksum in input_hashes(source).items()):
        raise RuntimeError("Dependency notices are stale; run python3 scripts/dependency_notices.py before packaging.")
    if "\nDependency inventory\n" not in content or "\nLicense texts\n" not in content or "\nText " not in content:
        raise RuntimeError("Dependency notices are incomplete; regenerate before packaging.")
    return path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="Check the committed notices against Cargo inputs")
    args = parser.parse_args()
    if args.check:
        validate(SOURCE)
    else:
        (SOURCE / FILENAME).write_text(generate(SOURCE), encoding="utf-8")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, RuntimeError, subprocess.SubprocessError) as error:
        sys.exit(f"Dependency notice generation failed: {error}")
