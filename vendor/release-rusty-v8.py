#!/usr/bin/env python3
"""Prepare the reviewed fork; publish only from the runner's matching version tag."""
import argparse
import hashlib
import io
import json
import os
import re
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
RECOVERY_COMMIT = "e178d4bc132bb6bc158d3d2bedf62891fe8c1c00"
RECOVERY_TAG = "v0.4.7"


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


def verify_source(source_root=ROOT):
    pins = json.loads((source_root / "RUSTY_V8_SOURCE_PINS.json").read_text())
    checks = {"Cargo.toml": "release_manifest_sha256", "src/isolate.rs": "patched_isolate_sha256",
              "LICENSE": "license_sha256", "tests/test_api.rs": "release_teardown_tests_sha256"}
    for path, pin in checks.items():
        if hashlib.sha256((source_root / "rusty_v8" / path).read_bytes()).hexdigest() != pins[pin]:
            raise ValueError("reviewed source hash changed: " + path)
    return pins


def prepare(output, offline=False, config=None, source_root=ROOT):
    verify_source(source_root)
    output.mkdir(parents=True, exist_ok=False)
    source = output / "source"
    shutil.copytree(source_root / "rusty_v8", source)
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


def registry_request(url):
    return urllib.request.Request(url, headers={"User-Agent": "turbo-test-reviewed-dependency-release"})


def published_archive():
    # Missing static objects can return 403. Only the registry's version API can
    # establish absence; a denied archive for an existing version must stay fatal.
    api = f"https://crates.io/api/v1/crates/{NAME}/{VERSION}"
    try:
        with urllib.request.urlopen(registry_request(api), timeout=30) as response:
            metadata = json.load(response)["version"]
    except urllib.error.HTTPError as error:
        error.close()
        if error.code == 404:
            return None
        raise
    if metadata.get("crate") != NAME or metadata.get("num") != VERSION:
        raise ValueError("registry returned a different fork identity")
    url = f"https://static.crates.io/crates/{NAME}/{NAME}-{VERSION}.crate"
    with urllib.request.urlopen(registry_request(url), timeout=30) as response:
        return response.read()


def checked_git(checkout, *args):
    return subprocess.check_output(["git", "-C", str(checkout), *args], text=True).strip()


def verify_recovery_source(source_root):
    expected = {"GITHUB_REPOSITORY": "miaskiewicz/turbo-test",
                "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF": "refs/heads/main"}
    if any(os.environ.get(key) != value for key, value in expected.items()):
        raise ValueError("recovery requires this repository's manual main workflow")
    publisher = os.environ.get("TURBO_TEST_RECOVERY_PUBLISHER_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", publisher) or os.environ.get("GITHUB_SHA") != publisher:
        raise ValueError("recovery publisher commit differs from the reviewed dispatch")
    checkout = source_root.parent
    if (source_root.name != "vendor"
            or checked_git(checkout, "rev-parse", "--show-toplevel") != str(checkout)
            or checked_git(checkout, "rev-parse", "HEAD") != RECOVERY_COMMIT):
        raise ValueError("recovery source is not the exact tagged checkout")
    if checked_git(checkout, "status", "--porcelain", "--untracked-files=all", "--ignored"):
        raise ValueError("recovery source contains changed or extra files")
    remote = checked_git(checkout, "ls-remote", "--tags", "origin", f"refs/tags/{RECOVERY_TAG}^{{}}")
    if remote.split() != [RECOVERY_COMMIT, f"refs/tags/{RECOVERY_TAG}^{{}}"]:
        raise ValueError("immutable recovery tag no longer identifies the reviewed source")


def verify_publish_context(source_root, recovery):
    runner = tomllib.loads((source_root.parent / "Cargo.toml").read_text())["package"]["version"]
    if recovery:
        if runner != "0.4.7":
            raise ValueError("recovery is restricted to runner v0.4.7")
        verify_recovery_source(source_root)
    elif source_root != ROOT or os.environ.get("GITHUB_REF") != "refs/tags/v" + runner:
        raise ValueError("publishing requires the matching runner version tag and its source")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", required=True, type=Path)
    parser.add_argument("--offline", action="store_true")
    parser.add_argument("--config", type=Path)
    parser.add_argument("--publish", action="store_true")
    parser.add_argument("--source-root", type=Path, default=ROOT)
    parser.add_argument("--recover-v047", action="store_true")
    args = parser.parse_args()
    source_root = args.source_root.resolve()
    if args.recover_v047 and not args.publish:
        parser.error("recovery requires an explicit publish operation")
    if args.publish:
        verify_publish_context(source_root, args.recover_v047)
        if args.offline or args.config:
            parser.error("publishing must use the real default registry without source replacement")
    output = args.out.resolve()
    source, package, env = prepare(output, args.offline, args.config, source_root)
    expected = package.read_bytes()
    receipt = {"name": NAME, "version": VERSION, "archiveSha256": hashlib.sha256(expected).hexdigest(),
               "archiveBytes": len(expected), "published": False, "existingPublishedSourceVerified": False}
    if args.recover_v047:
        receipt.update(sourceCommit=RECOVERY_COMMIT, sourceTag=RECOVERY_TAG,
                       recoveryPublisherCommit=os.environ["GITHUB_SHA"])
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
