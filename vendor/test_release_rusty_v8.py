"""Offline distribution checks for the reviewed Rusty V8 release alias."""
import hashlib
import importlib.util
import io
import tarfile
import json
from pathlib import Path
import tomllib
import unittest

ROOT = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("update_rusty_v8", ROOT / "update-rusty-v8.py")
UPDATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(UPDATE)
RELEASE_SPEC = importlib.util.spec_from_file_location("release_rusty_v8", ROOT / "release-rusty-v8.py")
RELEASE = importlib.util.module_from_spec(RELEASE_SPEC)
RELEASE_SPEC.loader.exec_module(RELEASE)


def package_bytes(changes=None):
    files = {"Cargo.toml": b'[package]\nname="turbo-surf-v8"\nversion="149.3.0"\n',
             "src/isolate.rs": b"reviewed runtime", "LICENSE": b"MIT license"}
    files.update(changes or {})
    stream = io.BytesIO()
    with tarfile.open(fileobj=stream, mode="w:gz") as package:
        for name, data in files.items():
            member = tarfile.TarInfo("turbo-surf-v8-149.3.0/" + name)
            member.size = len(data)
            package.addfile(member, io.BytesIO(data))
    return stream.getvalue()


class ReleaseRustyV8Tests(unittest.TestCase):
    def test_metadata_rename_keeps_library_version_and_native_contract(self):
        original = b'''[package]
name = "v8"
version = "149.3.0"
repository = "https://github.com/denoland/rusty_v8"
exclude = [
    "LICENSE*",
    "*.md",
]
[lib]
name = "v8"
path = "src/lib.rs"
'''
        result = tomllib.loads(UPDATE.release_manifest(original).decode())
        self.assertEqual(result["package"]["name"], "turbo-surf-v8")
        self.assertEqual(result["package"]["version"], "149.3.0")
        self.assertEqual(result["lib"], {"name": "v8", "path": "src/lib.rs"})
        self.assertEqual(result["package"]["exclude"], ["*.md"])
        self.assertEqual(result["package"]["metadata"]["turbo-surf-release"]["upstream-package"], "v8")

    def test_changed_upstream_markers_fail_instead_of_partial_rename(self):
        for changed in [b'v8', b'no matching upstream package metadata']:
            with self.assertRaises(ValueError):
                UPDATE.release_manifest(changed)

    def test_published_runtime_or_manifest_changes_are_rejected(self):
        for changes in [{"src/isolate.rs": b"unpatched runtime"},
                        {"Cargo.toml": b'[package]\nname="v8"\nversion="149.3.0"\n'},
                        {"LICENSE": b"other terms"}]:
            with self.assertRaises(ValueError):
                RELEASE.require_same_distribution(package_bytes(), package_bytes(changes))

    def test_only_generated_lock_and_vcs_metadata_can_differ(self):
        RELEASE.require_same_distribution(
            package_bytes({"Cargo.lock": b"old resolution", ".cargo_vcs_info.json": b"old commit"}),
            package_bytes({"Cargo.lock": b"new resolution", ".cargo_vcs_info.json": b"new commit"}),
        )

    def test_unsafe_archive_paths_are_rejected(self):
        with self.assertRaises(ValueError):
            RELEASE.archive_payload(package_bytes({"../escape": b"unexpected"}))

    def test_committed_candidate_keeps_exact_reviewed_source_and_license(self):
        pins = json.loads((ROOT / "RUSTY_V8_SOURCE_PINS.json").read_text())
        source = ROOT / "rusty_v8"
        for relative, pin in [("Cargo.toml", "release_manifest_sha256"),
                              ("src/isolate.rs", "patched_isolate_sha256"),
                              ("LICENSE", "license_sha256")]:
            self.assertEqual(hashlib.sha256((source / relative).read_bytes()).hexdigest(), pins[pin])
        manifest = tomllib.loads((source / "Cargo.toml").read_text())
        self.assertEqual(manifest["package"]["name"], "turbo-surf-v8")
        self.assertEqual(manifest["package"]["version"], pins["version"])
        self.assertEqual(manifest["lib"]["name"], "v8")
        self.assertNotIn("LICENSE*", manifest["package"]["exclude"])
        build = (source / "build.rs").read_text()
        self.assertIn('https://github.com/denoland/rusty_v8/releases/download', build)
        self.assertIn('env::var("CARGO_PKG_VERSION")', build)
        self.assertNotIn('env::var("CARGO_PKG_NAME")', build)


if __name__ == "__main__":
    unittest.main()
