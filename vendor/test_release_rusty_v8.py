"""Offline distribution checks for the reviewed Rusty V8 release alias."""
import hashlib
import importlib.util
import io
import tarfile
import tempfile
import json
from pathlib import Path
import tomllib
import unittest
from unittest import mock
import urllib.error

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


    def test_missing_version_api_404_does_not_request_a_static_archive(self):
        error = urllib.error.HTTPError("api", 404, "missing", {}, None)
        with mock.patch.object(RELEASE.urllib.request, "urlopen", side_effect=error) as request:
            self.assertIsNone(RELEASE.published_archive())
        self.assertEqual(request.call_count, 1)
        self.assertEqual(request.call_args.args[0].full_url,
                         "https://crates.io/api/v1/crates/turbo-surf-v8/149.3.0")

    def test_present_version_downloads_the_exact_static_archive(self):
        metadata = io.BytesIO(b'{"version":{"crate":"turbo-surf-v8","num":"149.3.0"}}')
        archive = package_bytes()
        with mock.patch.object(RELEASE.urllib.request, "urlopen",
                               side_effect=[metadata, io.BytesIO(archive)]) as request:
            self.assertEqual(RELEASE.published_archive(), archive)
        self.assertEqual(request.call_count, 2)
        self.assertEqual(request.call_args.args[0].full_url,
                         "https://static.crates.io/crates/turbo-surf-v8/turbo-surf-v8-149.3.0.crate")

    def test_api_or_existing_archive_403_remains_a_failure(self):
        for after_metadata in [False, True]:
            error = urllib.error.HTTPError("denied", 403, "denied", {}, None)
            calls = [error]
            if after_metadata:
                calls.insert(0, io.BytesIO(b'{"version":{"crate":"turbo-surf-v8","num":"149.3.0"}}'))
            with mock.patch.object(RELEASE.urllib.request, "urlopen", side_effect=calls):
                with self.assertRaises(urllib.error.HTTPError):
                    RELEASE.published_archive()
            error.close()

    def test_wrong_registry_version_is_rejected_before_archive_download(self):
        metadata = io.BytesIO(b'{"version":{"crate":"v8","num":"149.3.0"}}')
        with mock.patch.object(RELEASE.urllib.request, "urlopen", return_value=metadata) as request:
            with self.assertRaises(ValueError):
                RELEASE.published_archive()
        self.assertEqual(request.call_count, 1)

    def recovery_environment(self):
        return {"GITHUB_REPOSITORY": "miaskiewicz/turbo-test",
                "GITHUB_EVENT_NAME": "workflow_dispatch", "GITHUB_REF": "refs/heads/main",
                "GITHUB_SHA": "a" * 40, "TURBO_TEST_RECOVERY_PUBLISHER_SHA": "a" * 40}

    def recovery_git_outputs(self):
        return [str(ROOT.parent), RELEASE.RECOVERY_COMMIT, "",
                RELEASE.RECOVERY_COMMIT + "\trefs/tags/v0.4.7^{}"]

    def test_recovery_accepts_only_exact_clean_tagged_source(self):
        with mock.patch.dict(RELEASE.os.environ, self.recovery_environment(), clear=True), \
                mock.patch.object(RELEASE, "checked_git", side_effect=self.recovery_git_outputs()) as git:
            RELEASE.verify_recovery_source(ROOT)
        self.assertEqual(git.call_args.args[1:],
                         ("ls-remote", "--tags", "origin", "refs/tags/v0.4.7^{}"))

    def test_recovery_rejects_wrong_repository_event_ref_and_publisher(self):
        for key in self.recovery_environment():
            env = self.recovery_environment()
            env[key] = "wrong"
            with mock.patch.dict(RELEASE.os.environ, env, clear=True), \
                    mock.patch.object(RELEASE, "checked_git") as git:
                with self.assertRaises(ValueError):
                    RELEASE.verify_recovery_source(ROOT)
                git.assert_not_called()

    def test_recovery_rejects_wrong_source_extra_files_and_moved_tag(self):
        for index, changed in [(0, "/other/checkout"), (1, "b" * 40),
                               (2, "?? extra-runtime-source"),
                               (3, "b" * 40 + "\trefs/tags/v0.4.7^{}")]:
            outputs = self.recovery_git_outputs()
            outputs[index] = changed
            with mock.patch.dict(RELEASE.os.environ, self.recovery_environment(), clear=True), \
                    mock.patch.object(RELEASE, "checked_git", side_effect=outputs):
                with self.assertRaises(ValueError):
                    RELEASE.verify_recovery_source(ROOT)

    def test_normal_publishing_still_requires_its_matching_tag_and_default_source(self):
        runner = tomllib.loads((ROOT.parent / "Cargo.toml").read_text())["package"]["version"]
        with mock.patch.dict(RELEASE.os.environ, {"GITHUB_REF": "refs/tags/v" + runner}, clear=True):
            RELEASE.verify_publish_context(ROOT, False)
        with mock.patch.dict(RELEASE.os.environ, {"GITHUB_REF": "refs/heads/main"}, clear=True):
            with self.assertRaises(ValueError):
                RELEASE.verify_publish_context(ROOT, False)
        with tempfile.TemporaryDirectory() as temporary:
            checkout = Path(temporary)
            source = checkout / "vendor"
            source.mkdir()
            (checkout / "Cargo.toml").write_text('[package]\nversion="' + runner + '"\n')
            with mock.patch.dict(RELEASE.os.environ, {"GITHUB_REF": "refs/tags/v" + runner}, clear=True):
                with self.assertRaises(ValueError):
                    RELEASE.verify_publish_context(source, False)
            (checkout / "Cargo.toml").write_text('[package]\nversion="0.4.8"\n')
            with mock.patch.dict(RELEASE.os.environ, self.recovery_environment(), clear=True), \
                    mock.patch.object(RELEASE, "checked_git") as git:
                with self.assertRaises(ValueError):
                    RELEASE.verify_publish_context(source, True)
                git.assert_not_called()

    def test_committed_candidate_keeps_exact_reviewed_source_and_license(self):
        pins = json.loads((ROOT / "RUSTY_V8_SOURCE_PINS.json").read_text())
        source = ROOT / "rusty_v8"
        for relative, pin in [("Cargo.toml", "release_manifest_sha256"),
                              ("src/isolate.rs", "patched_isolate_sha256"),
                              ("src/function.rs", "patched_function_sha256"),
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
