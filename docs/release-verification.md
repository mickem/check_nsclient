# Verifying releases

Releases produced by the updated workflow include:

- A CycloneDX 1.5 JSON SBOM for each platform, named
  `check_nsclient-<version>-<platform>.cdx.json`.
- `SHA256SUMS`, containing SHA-256 hashes of all five binaries and their SBOMs.
- GitHub build provenance attestations for the binaries, SBOMs, and `SHA256SUMS`,
  plus an SBOM attestation binding each binary to its target's SBOM.
- `attestations.jsonl`, containing the signed Sigstore bundles also stored on GitHub.

The platforms are `windows-x64`, `windows-x86`, `windows-arm64`, `linux-x64`, and
`linux-arm64`. Windows binaries have an `.exe` suffix. Checksums and attestations
cover the final files, including the Windows Authenticode signatures.

The SBOMs describe the Cargo dependency graph for the build's target and default
features, including transitive dependencies. Development-only dependencies are
omitted; build-only dependencies are marked as excluded by cargo-cyclonedx. These
are Cargo SBOMs and do not inventory system libraries or the runner's toolchain.

## Verify a download

Use a current [GitHub CLI](https://cli.github.com/) and authenticate with
`gh auth login` (or set `GH_TOKEN` in automation). Choose a published version that
includes these assets; older releases are not retroactively attested.

The following Bash commands download a Linux x64 binary and its verification
assets into the current directory. Replace `VERSION` with the exact release tag.

```bash
set -euo pipefail
VERSION='<release-tag>'
REPO='mickem/check_nsclient'
ASSET="check_nsclient-$VERSION-linux-x64"
SIGNER="$REPO/.github/workflows/release.yml"

gh release download "$VERSION" --repo "$REPO" \
  --pattern "$ASSET" --pattern "$ASSET.cdx.json" \
  --pattern SHA256SUMS --pattern attestations.jsonl

# Authenticate the checksum manifest before trusting its contents.
gh attestation verify SHA256SUMS --repo "$REPO" \
  --signer-workflow "$SIGNER" --source-ref refs/heads/main \
  --bundle attestations.jsonl

# Require exactly the two files we downloaded; missing entries must fail.
awk -v binary="$ASSET" -v sbom="$ASSET.cdx.json" \
  '$2 == "*" binary { print; binaries++ }
   $2 == "*" sbom { print; sboms++ }
   END { if (binaries != 1 || sboms != 1) exit 1 }' \
  SHA256SUMS > selected.sha256
sha256sum --check --strict selected.sha256

# Verify the binary's provenance and its signed SBOM claim.
gh attestation verify "$ASSET" --repo "$REPO" \
  --signer-workflow "$SIGNER" --source-ref refs/heads/main \
  --bundle attestations.jsonl
gh attestation verify "$ASSET" --repo "$REPO" \
  --signer-workflow "$SIGNER" --source-ref refs/heads/main \
  --bundle attestations.jsonl --predicate-type https://cyclonedx.org/bom
```

With all ten files downloaded, use `sha256sum --check --strict SHA256SUMS` to check
them together. On macOS, use `shasum -a 256 --check selected.sha256` instead of
`sha256sum`. On Windows, use `Get-FileHash -Algorithm SHA256` to compare a file's
hash with its entry in the authenticated manifest; the same `gh attestation
verify` options work in PowerShell.

Omit `--bundle attestations.jsonl` to fetch attestations from GitHub instead.
The bundle does not appear in `SHA256SUMS`: its embedded signatures authenticate
it, and including it would create a cycle with the attested checksum manifest.

## Use in downstream CI

Run the verification commands before executing or packaging a downloaded binary.
For GitHub Actions, set `GH_TOKEN: ${{ github.token }}` in the step's environment
and use `shell: bash`; `contents: read` is sufficient for the consuming workflow
when downloading these public release assets.

Keep the repository, signer workflow, and source ref restrictions. The signer is
the reusable `release.yml` workflow. Releases are built on pushes to `main`, so
the attested source ref is `refs/heads/main`, even though the download uses a
release tag. To pin the source commit as well, add
`--source-digest <expected-full-commit-sha>` to each verification command, using a
commit reviewed and recorded by your project.

A matching SHA-256 hash detects changed bytes. A verified attestation also checks
the signing identity and the artifact digest. See the
[GitHub CLI verification reference](https://cli.github.com/manual/gh_attestation_verify)
for additional verification policies.

## Maintaining the workflow

`build-rust.yml` generates each SBOM after a locked build, using the release
version and the same target triple. It checks that SBOM generation did not change
`Cargo.lock`. `release.yml` collects the signed binaries and SBOMs from that run,
then generates checksums and attestations before creating the draft release.

Only the release job in `build-main.yml` grants `id-token: write` and
`attestations: write` to the reusable release workflow. Signing uses GitHub OIDC
and requires no additional signing secret. Feature builds generate SBOMs but do
not publish attestations. Generation, attestation, and asset upload failures fail
the release job.

## Dependency and download verification

There is no single lockfile covering every kind of download:

| Input | Pinning and verification |
| --- | --- |
| External GitHub Actions | Full upstream commit SHAs, with version comments for maintenance. Local reusable workflows come from the caller's commit. |
| Rust dependencies | The committed `Cargo.lock` records versions and registry checksums. Builds, Clippy, and integration tests use `--locked`. |
| SBOM generator | `cargo-cyclonedx` is fixed at 0.5.9 and installed with `--locked`, using the tool's published lockfile. |
| Versioning tool | `git-version` 2.8.7 is checked against the SHA-256 recorded in `get-version.yml` before execution. |
| NSClient++ test packages | `tests/integration/downloads.sha256` records hashes for the Windows ZIPs and Linux DEBs. Downloads and cached ZIPs are verified before use. |
| Build artifacts passed to the release job | Downloads are scoped to the same workflow run. GitHub checks artifact digests, but a mismatch is reported as a warning by the download Action. These artifacts are not Cargo dependencies. |

Dependabot checks Action pins and the root Cargo manifest weekly. Review pin and
checksum changes as code changes; do not regenerate expected hashes automatically
during CI. For a new NSClient++ version, add verified checksums before changing
`.nscp_version` or using a version override. The initial NSClient++ and git-version
hashes were taken from their upstream GitHub release asset metadata.

The Rust `stable` channel, hosted runner images, Ubuntu container tag, OS packages,
and tools downloaded internally by third-party Actions are not all locked by
this repository. An Action SHA fixes that Action's source; it does not freeze
everything the Action downloads. See GitHub's
[Action pinning guidance](https://docs.github.com/en/actions/reference/security/secure-use#using-third-party-actions)
and [artifact digest verification](https://docs.github.com/en/actions/tutorials/store-and-share-data#validating-artifacts).
