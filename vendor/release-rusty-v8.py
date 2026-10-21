#!/usr/bin/env python3
"""Prepare the reviewed fork; publish only from the runner's matching version tag."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import time
import tomllib
import urllib.error
import urllib.request

ROOT = Path(__file__).resolve().parent
NAME = "turbo-surf-v8"
VERSION = "149.3.0"


def archive_payload(raw):
    payload = {}
    with tarfile.open(fileobj=io.BytesIO(raw), mode="r:gz") as archive:
        for member in archive:
            if member.isdir():
                continue
            path = PurePosixPath(member.name)
            if (not member.isfile() or path.is_absolute() or ".." in path.parts
                    or not path.parts or path.parts[0] != f"{NAME}-{VERSION}"):
                raise ValueError("unsafe or unexpected package member")
            relative = str(PurePosixPath(*path.parts[1:]))
            if relative in payload:
                raise ValueError("duplicate package member")
            if relative not in {"Cargo.lock", ".cargo_vcs_info.json"}:
                payload[relative] = archive.extractfile(member).read()
    manifest = tomllib.loads(payload["Cargo.toml"].decode())
    if manifest["package"]["name"] != NAME or manifest["package"]["version"] != VERSION:
        raise ValueError("wrong published fork identity")
    if "LICENSE" not in payload or "src/isolate.rs" not in payload:
        raise ValueError("missing license or reviewed runtime source")
    return payload


def require_same_distribution(expected, actual):
    left, right = archive_payload(expected), archive_payload(actual)
    different = sorted(path for path in left.keys() | right.keys()
                       if left.get(path) != right.get(path))
    if different:
        raise ValueError("published fork differs: " + ", ".join(different))


def verify_source():
    pins = json.loads((ROOT / "RUSTY_V8_SOURCE_PINS.json").read_text())
    checks = {"Cargo.toml": "release_manifest_sha256", "src/isolate.rs": "patched_isolate_sha256",
              "LICENSE": "license_sha256", "tests/test_api.rs": "release_teardown_tests_sha256"}
    for path, pin in checks.items():
        if hashlib.sha256((ROOT / "rusty_v8" / path).read_bytes()).hexdigest() != pins[pin]:
            raise ValueError("reviewed source hash changed: " + path)
    return pins


def prepare(output, offline=False, config=None):
    verify_source()
    output.mkdir(parents=True, exist_ok=False)
    source = output / "source"
    shutil.copytree(ROOT / "rusty_v8", source)
    # Cargo regenerates these metadata files; upstream's reserved .orig is invalid input.
    for name in ["Cargo.lock", ".cargo_vcs_info.json", "Cargo.toml.orig"]:
        (source / name).unlink(missing_ok=True)
    command = ["cargo", "package", "--manifest-path", str(source / "Cargo.toml"),
               "--allow-dirty", "--no-verify", "--exclude-lockfile"]
    if offline:
        command.append("--offline")
    if config:
        command += ["--config", str(config)]
    env = dict(os.environ, CARGO_TARGET_DIR=str(output / "target"))
    with (output / "package.log").open("w") as log:
        subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT, check=True)
    package = output / "target/package" / f"{NAME}-{VERSION}.crate"
    archive_payload(package.read_bytes())
    return source, package, env


def published_archive():
    url = f"https://static.crates.io/crates/{NAME}/{NAME}-{VERSION}.crate"
    request = urllib.request.Request(url, headers={"User-Agent": "turbo-test-reviewed-dependency-release"})
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read()
    except urllib.error.HTTPError as error:
        if error.code == 404:
            return None
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--config", type=Path)
    parser.add_argument("--publish", action="store_true")
    args = parser.parse_args()
    if args.publish:
        runner = tomllib.loads((ROOT.parent / "Cargo.toml").read_text())["package"]["version"]
        if os.environ.get("GITHUB_REF") != "refs/tags/v" + runner:
            parser.error("publishing requires the matching runner version tag")
        if args.offline or args.config:
            parser.error("publishing must use the real default registry without source replacement")
    output = args.out.resolve()
    source, package, env = prepare(output, args.offline, args.config)
    expected = package.read_bytes()
    receipt = {"name": NAME, "version": VERSION, "archiveSha256": hashlib.sha256(expected).hexdigest(),
               "archiveBytes": len(expected), "published": False, "existingPublishedSourceVerified": False}
    if args.publish:
        actual = published_archive()
        if actual is not None:
            require_same_distribution(expected, actual)
            receipt["existingPublishedSourceVerified"] = True
        else:
            # Keep Cargo's actual packaged-consumer verification enabled.
            subprocess.run(["cargo", "publish", "--manifest-path", str(source / "Cargo.toml"),
                            "--allow-dirty"], env=env, check=True)
            for _ in range(40):
                actual = published_archive()
                if actual is not None:
                    require_same_distribution(expected, actual)
                    receipt["published"] = True
                    break
                time.sleep(3)
            else:
                raise RuntimeError("published fork archive not observable within 120 seconds")
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
