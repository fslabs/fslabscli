<div align="center">

# FSLABSCLI

## License

fslabscli is free and open source! All code in this repository is dual-licensed under either:

* MIT License ([LICENSE-MIT](LICENSE-MIT) or [http://opensource.org/licenses/MIT](http://opensource.org/licenses/MIT))
* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or [http://www.apache.org/licenses/LICENSE-2.0](http://www.apache.org/licenses/LICENSE-2.0))

at your option. This means you can select the license you prefer! This dual-licensing approach is the de-facto standard in the Rust ecosystem and there are very good reasons to include both.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
</div>

## Installation

To install, run the following command:
``cargo install --git https://github.com/fslabs/fslabscli``

## Release Process

**Version source of truth:** `Cargo.toml`

Releases are cut by pushing a release branch, not by merging to `main`. Merging to `main` runs
tests only and triggers no publish job. The release trigger is a branch whose name matches the
`publish` postsubmit's branch pattern configured in fslabs-infra
(`modules/ci/prow/terragrunt.hcl`, currently `^cargo-fslabscli-\d+\.\d+\.\d+$`).

### Steps

1. **Bump version** — Update `version` in `Cargo.toml` (and the `cargo-fslabscli` entry in
   `Cargo.lock`), open a PR, merge to `main`. Changelog categories for the generated release notes
   are controlled by [.github/release.yml](.github/release.yml).

2. **Push the release branch** — From the merged `main`, push a branch named
   `cargo-fslabscli-{version}` (e.g. `cargo-fslabscli-2.46.0`). This fires the Prow `publish-all`
   postsubmit, which runs `fslabscli publish` and:
   - Builds the release binaries
   - Publishes the crate to the `fsl` Cargo registry
   - Builds and pushes the Docker image
   - Creates the GitHub release and its tag

3. **Downstream** — Kargo detects the new tag and starts canary promotion. In fslabs-infra,
   updatecli bumps `fslabscli_version` in the prow-tests image so CI uses the new binary.

### Flow

``` mermaid
sequenceDiagram
    actor Dev as Developer
    participant GH as GitHub
    participant Prow as Prow (publish-all)
    participant Rel as GitHub Release
    participant Kargo as Kargo

    Dev->>GH: Merge PR (version bump in Cargo.toml)
    Dev->>GH: Push branch cargo-fslabscli-{version}
    GH->>Prow: Postsubmit on the cargo-fslabscli-* branch
    Prow->>Prow: Build binaries, publish crate to fsl registry
    Prow->>Prow: Build & push Docker image
    Prow->>Rel: Create release + tag
    Rel->>Kargo: Tag detected
    Kargo->>Kargo: Start canary promotion
```
