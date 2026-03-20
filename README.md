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
**Tag format:** `cargo-fslabscli-{version}` (e.g., `cargo-fslabscli-2.42.0`)

### Steps

1. **Bump version** — Update `version` in `Cargo.toml`, open a PR, merge to `main`. Label the PR appropriately (Features, Bug Fixes, Maintenance, Documentation) — PRs with `skip-changelog` label are excluded from release notes.

2. **Draft release auto-created** — On merge, the [Release Drafter](.github/workflows/release-drafter.yml) action creates or updates a draft GitHub Release tagged `cargo-fslabscli-{version}`. Changelog is generated from PR labels (config: [release-drafter.yaml](.github/release-drafter.yaml)).

3. **Publish the draft** — Review the draft release on GitHub. Click **"Publish release"** — this creates the git tag.

4. **Prow builds and publishes** — Publishing triggers a webhook to Prow, which runs `publish-all`:
   - Publishes crate to Cargo registr
   - Builds Nix binary
   - Uploads binary artifacts to the GitHub Release

5. **Mark as latest** — Wait for Prow to finish uploading artifacts. **Do not mark as latest until all assets are available on the release.** Once verified, mark the release as **"latest"**.

### Flow

``` mermaid
sequenceDiagram
    actor Dev as Developer
    participant GH as GitHub (main)
    participant RD as Release Drafter Action
    participant Rel as GitHub Release
    participant Prow as Prow (publish-all)

    Dev->>GH: Merge PR (version bump in Cargo.toml)
    GH->>RD: Trigger on push to main
    RD->>Rel: Create/update draft release<br/>tag: cargo-fslabscli-{version}<br/>changelog from PR labels

    Dev->>Rel: Review draft, click "Publish release"
    Note over Rel: Git tag created

    Rel->>Prow: Webhook (release published)
    Prow->>Prow: Publish crate to Cargo registry (fsl)
    Prow->>Prow: Build Nix binary (--fallback)
    Prow->>Prow: Push to Nix cache (atticd)
    Prow->>Prow: Build & push Docker image
    Prow->>Rel: Upload binary artifacts

    Dev->>Rel: Verify artifacts, mark as "latest"
```
