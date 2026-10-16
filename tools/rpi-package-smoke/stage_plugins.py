#!/usr/bin/env python
"""Stage exactly this workspace's extension cdylibs for the smoke tests.

The smoke tests load a *directory* of plugins, so pointing them at
`target/release` makes the result depend on unrelated build history: a stale
`rpi_orchestrator.dll` left over from another branch changes the discovered tool
set and fails a test that has nothing to do with it.

This copies only the cdylibs that the current workspace declares into a staging
directory (empty first), so the loaded set is a property of the checkout rather
than of whatever was built last. The output must not contain the build inputs.

Usage: stage_plugins.py [--target-dir DIR] [--out DIR]
       stage_plugins.py --print-release-dir
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def ext_for(platform: str) -> str:
    if platform == "win32":
        return ".dll"
    if platform == "darwin":
        return ".dylib"
    return ".so"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--target-dir", help="cargo target dir (default: cargo's own)")
    parser.add_argument("--out", help="staging directory")
    parser.add_argument("--print-release-dir", action="store_true", help="print Cargo's resolved release directory")
    args = parser.parse_args()
    if not args.print_release_dir and not args.out:
        parser.error("--out is required when staging plugins")

    metadata = json.loads(
        subprocess.check_output(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            # Explicit UTF-8: the Windows default codepage cannot decode paths
            # that cargo prints for non-ASCII workspace members.
            encoding="utf-8",
        )
    )
    target_dir = args.target_dir or metadata["target_directory"]
    release = os.path.join(target_dir, "release")
    if args.print_release_dir:
        print(release)
        return 0
    output_path = Path(args.out).resolve()
    protected_paths = [Path(release).resolve()]
    if metadata.get("workspace_root"):
        protected_paths.append(Path(metadata["workspace_root"]).resolve())
    # Validate the resolved paths before emptying staging: --out=target/release
    # (or one of its ancestors) would otherwise erase the files being copied.
    if any(output_path == protected or output_path in protected.parents
           for protected in protected_paths):
        parser.error("--out must not contain the release directory or workspace root")
    prefix = "" if sys.platform == "win32" else "lib"
    suffix = ext_for(sys.platform)

    # A workspace member that builds a cdylib *is* an extension.
    names = sorted(
        {
            target["name"].replace("-", "_")
            for package in metadata["packages"]
            for target in package["targets"]
            if "cdylib" in target.get("crate_types", [])
        }
    )

    if os.path.isdir(args.out):
        shutil.rmtree(args.out)
    os.makedirs(args.out)

    missing = []
    for name in names:
        source = os.path.join(release, f"{prefix}{name}{suffix}")
        if os.path.exists(source):
            shutil.copy2(source, os.path.join(args.out, os.path.basename(source)))
        else:
            missing.append(os.path.basename(source))

    print(f"staged {len(names) - len(missing)}/{len(names)} cdylibs into {args.out}")
    if missing:
        # Not fatal: a cdylib that did not build is a real failure the loader
        # assertions will report with a better message than we could here.
        print("missing (not built?): " + ", ".join(missing), file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
