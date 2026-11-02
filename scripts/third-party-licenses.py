#!/usr/bin/env python3
"""Writes the licenses of the crates linked into Shroom Stack's binaries.

Follows the normal (not dev or build) dependencies of shroom-ui and shroom-cli for one
target platform, collects each crate's license files from Cargo's package cache and
lists every distinct license text once, with the crates using it.

    python3 scripts/third-party-licenses.py -o THIRD-PARTY-LICENSES.txt [--target <triple>]
"""

import argparse
import json
import re
import subprocess
from pathlib import Path

ROOTS = ("shroom-ui", "shroom-cli")
# File names (case-insensitive prefixes) that hold license texts or notices.
LICENSE_FILES = ("license", "licence", "copying", "copyright", "notice", "unlicense")
# Crates that keep license texts elsewhere: glob patterns in the package.
EXTRA_FILES = {"epaint_default_fonts": ["fonts/*.txt"]}
RAWLER_NOTE = """\
rawler (raw image decoding) is licensed under the GNU Lesser General Public License,
version 2.1, and is linked into this program. Its source code is available at
https://github.com/dnglab/dnglab and https://crates.io/crates/rawler (version {version}).
Shroom Stack's own source code is available at its repository, so the program can be
rebuilt against a modified version of rawler with Cargo.
"""


# Standard texts for crates that ship no license file (Apache-2.0 is read from the
# repository's LICENSE-APACHE).
MIT_TEXT = """\
MIT License

Copyright (c) the authors of the crates listed above

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."""
BSD_3_TEXT = """\
BSD 3-Clause License

Copyright (c) the authors of the crates listed above

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE."""


def standard_text(license_id):
    if license_id == "MIT":
        return MIT_TEXT
    if license_id == "BSD-3-Clause":
        return BSD_3_TEXT
    if license_id == "Apache-2.0":
        return (Path(__file__).resolve().parent.parent / "LICENSE-APACHE").read_text(encoding="utf-8").strip()
    return f"See https://spdx.org/licenses/{license_id}.html"


def chosen_license(expression):
    """The license used from an SPDX expression: MIT or Apache-2.0 where offered."""
    options = [o.strip("() ") for o in re.split(r" OR |/", expression or "")]
    for preferred in ("MIT", "Apache-2.0"):
        if preferred in options:
            return preferred
    return options[0] if options and options[0] else "unknown"


def host_target():
    out = subprocess.check_output(["rustc", "-vV"], text=True)
    return re.search(r"^host: (\S+)", out, re.M).group(1)


def dependencies(target):
    """Packages reachable from the binaries through normal dependencies on `target`."""
    meta = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target], text=True
        )
    )
    packages = {p["id"]: p for p in meta["packages"]}
    members = set(meta["workspace_members"])
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    todo = [i for i in members if packages[i]["name"] in ROOTS]
    seen = set()
    while todo:
        node = todo.pop()
        if node in seen:
            continue
        seen.add(node)
        for dep in nodes[node]["deps"]:
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    deps = [packages[i] for i in seen - members]
    return sorted(deps, key=lambda p: (p["name"], p["version"]))


def license_files(package):
    root = Path(package["manifest_path"]).parent
    files = [f for f in root.iterdir() if f.is_file() and f.name.lower().startswith(LICENSE_FILES)]
    if package.get("license_file"):
        files.append(root / package["license_file"])
    for pattern in EXTRA_FILES.get(package["name"], []):
        files.extend(root.glob(pattern))
    return sorted(set(files))


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("-o", "--output", required=True, type=Path)
    parser.add_argument("--target", help="target triple (default: the host)")
    args = parser.parse_args()

    deps = dependencies(args.target or host_target())
    # Each distinct text (ignoring whitespace differences) with the crates using it.
    texts = {}
    missing = []
    for package in deps:
        crate = f"{package['name']} {package['version']} ({package.get('license') or 'license not stated'})"
        files = license_files(package)
        if not files:
            missing.append((package, crate))
        for f in files:
            text = f.read_text(encoding="utf-8", errors="replace").strip()
            key = " ".join(text.split())
            texts.setdefault(key, (text, []))[1].append(crate)

    rawler = next((p for p in deps if p["name"] == "rawler"), None)
    out = [
        "Third-party software in Shroom Stack",
        "====================================",
        "",
        "Shroom Stack itself is licensed under the MIT license or the Apache License 2.0, at",
        "your option (see LICENSE-MIT and LICENSE-APACHE). It includes the following",
        f"third-party components ({len(deps)} crates).",
        "",
    ]
    if rawler:
        out += [RAWLER_NOTE.format(version=rawler["version"])]
    # Crates without a license file, under the standard text of the license they state.
    standard = {}
    for package, crate in missing:
        standard.setdefault(chosen_license(package.get("license")), []).append(crate)
    for license_id, crates in sorted(standard.items()):
        out += ["-" * 80, "", f"Used under {license_id} (these crates ship no license file):"]
        out += [f"  - {c}" for c in crates] + ["", standard_text(license_id), ""]
    for text, crates in sorted(texts.values(), key=lambda t: (-len(t[1]), t[1][0])):
        out += ["-" * 80, "", "Used by:"] + [f"  - {c}" for c in crates] + ["", text, ""]
    args.output.write_text("\n".join(out), encoding="utf-8")
    print(f"{args.output}: {len(deps)} crates, {len(texts)} license texts, {len(missing)} without a file")


if __name__ == "__main__":
    main()
