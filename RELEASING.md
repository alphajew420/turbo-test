# Releasing turbo-test

The release publishes `@miaskiewicz/turbo-test` on npm and `turbo-test` on
crates.io. This repository has no PyPI release. The `vX.Y.Z` tag runs the four
supported binary builds, their tests, and both registry publications.

Before release, merge all reviewed stacked work into the final release PR. Bump
`Cargo.toml`, the runner entry in `Cargo.lock`, `package.json` and both root
versions in `package-lock.json` to the same next patch version. Move the complete
unreleased changelog into that release's dated section. Preserve earlier versions
and unrelated dependency versions.

## Native dependency provenance

The runner uses the reviewed `turbo-surf-v8` 149.3.0 distribution through its local
`v8` alias. Its numeric version, library name and official upstream prebuilt SDK
URLs remain unchanged. The fork retains the Rust weak-handle teardown fix;
falling back to the unpatched `v8` registry package is not equivalent.

```sh
python3 -B vendor/update-rusty-v8.py --check
python3 -B vendor/test_release_rusty_v8.py
python3 -B vendor/release-rusty-v8.py --out /tmp/reviewed-v8-package --offline
```

The archive/patch reproducer uses the hash-pinned original archive from Cargo's
cache, or its explicit `--archive` argument. Packaging works on an owned source
copy, omitting only regenerated Cargo metadata. The actual package retains its
license and reviewed source. Keep the actual prepared archive and independent
normal/snapshot weak-teardown test evidence. A directory-source clean-consumer
check before publication is useful preparation; it is not a claim that the new
fork already exists on crates.io.

The tagged workflow publishes the verified V8 fork before either runner
publication. An existing fork version is accepted only if its package contents
match exactly, apart from Cargo-generated lock/VCS metadata. After the fork is
published, the Rust job verifies the normalized runner package against real
registry dependencies before publishing `turbo-test`. Cargo publication
verification remains enabled. The jobs use the existing `CARGO_REGISTRY_TOKEN`
and `NPM_TOKEN` secrets; never record token values.

Use a published compatible `turbo-dom` floor before the runner crate release.
A Git-pinned development version with the same number does not prove that its
required APIs are available to registry consumers.

## Gates and final merge tag

Run the complete locked Rust tests, the Node compatibility/type tests, and all
nine native DOM compatibility fixtures used by `.github/workflows/ci.yml`. Check
formatting for changed Rust sources and report any additional Clippy checks with
their actual outcomes rather than relabeling an older failing baseline. Keep the explicit unknown-pseudo CSSOM admission
ignore and its separately executed Chrome-expected failure visible; do not count
it as passing conformance. Check the actual normalized runner package from a
clean consumer and inspect the npm allowlist and staged platform binaries.

Merge all open reviewed PRs with merge commits. Require the integrated final PR's
four platform checks to pass. Tag the **final main merge commit**, once, only after
all intended changes have landed:

```sh
git tag -a vX.Y.Z FINAL_MERGE_SHA -m vX.Y.Z
git push origin vX.Y.Z
```

No separate Python tag exists here. Verify the tagged workflow and both actual
registries after publication; do not infer success from a green build job or
from an existing older package. Keep each failed attempt with its actual source
identity. A tag is an irreversible publication request.

## Recovering the immutable v0.4.7 dependency publication

The original tagged run passed all four platform jobs and complete source
reproduction, then stopped before publishing: an absent static crate object
returned HTTP 403. Absence is now determined only by the official registry
version API's 404 response. An API denial or a denied archive for an existing
version remains a failure; full archive payload comparison is retained.

Do not move or recreate `v0.4.7`. The manual `Recover tagged V8 dependency`
workflow can publish or verify only `turbo-surf-v8` 149.3.0 from exact source
`e178d4bc132bb6bc158d3d2bedf62891fe8c1c00`. It requires the reviewed publisher's
full main commit SHA and the explicit confirmation shown in its dispatch form.
It independently checks the clean tagged checkout and the remote annotated tag's
peeled commit, reproduces every fork source, and keeps Cargo verification enabled.
The publisher and tagged source are separate checkouts; no source overlay or
local registry credential is used.

After reviewing and merging the recovery PR, dispatch the workflow on `main`
with that exact publisher commit. Verify the resulting registry archive before
rerunning the failed jobs of original tagged run 39585126739. The original
publisher must then verify the existing fork payload before allowing npm and
runner-crate publication. This is one product release, not a second version or
replacement tag. Preserve the original failed run and every recovery outcome.
