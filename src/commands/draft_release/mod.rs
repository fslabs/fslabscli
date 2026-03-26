use std::{
    fmt::{Display, Formatter},
    path::PathBuf,
};

use anyhow::Context;
use clap::Parser;
use octocrab::Octocrab;
use serde::Serialize;

use crate::PrettyPrintable;
use crate::commands::release_utils::{format_tag, upload_artifacts_to_release};
use crate::utils::github::{InstallationRetrievalMode, generate_github_app_token};

/// Creates or finds a draft GitHub release and uploads artifacts to it.
///
/// When `--release-tag` is provided it is used directly. Otherwise the tag is
/// derived from `--tag-format`, `--package-name`, and `--version`.
///
/// The command is idempotent: if a draft release with the computed tag already
/// exists its assets are updated; if none exists a new draft is created.
#[derive(Debug, Parser, Clone)]
#[command(about = "Create or update a draft GitHub release and upload artifacts")]
pub struct Options {
    #[arg(long, env)]
    pub repo_owner: String,
    #[arg(long, env)]
    pub repo_name: String,
    #[arg(long, env)]
    pub github_app_id: Option<u64>,
    #[arg(long, env)]
    pub github_app_private_key: Option<PathBuf>,
    /// Directory containing artifacts to upload.
    #[arg(long, env, default_value = ".")]
    pub artifacts: PathBuf,
    /// Use this tag directly instead of deriving one from package name/version.
    #[arg(long, env)]
    pub release_tag: Option<String>,
    /// Template for deriving the release tag.
    /// Supports `{package_name}` and `{version}` as placeholders.
    #[arg(long, env, default_value = "{package_name}-{version}")]
    pub tag_format: String,
    /// Package name, used together with `--tag-format` when `--release-tag` is absent.
    #[arg(long, env, default_value = "")]
    pub package_name: String,
    /// Package version, used together with `--tag-format` when `--release-tag` is absent.
    #[arg(long, env, default_value = "")]
    pub version: String,
    /// Release title. Defaults to the tag name when not set.
    #[arg(long, env)]
    pub release_name: Option<String>,
}

#[derive(Serialize, Default, Clone)]
pub struct DraftReleaseResult {
    pub tag: String,
    pub release_id: u64,
    pub uploaded_assets: Vec<String>,
}

impl Display for DraftReleaseResult {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Draft release: {} (id={})", self.tag, self.release_id)?;
        if self.uploaded_assets.is_empty() {
            writeln!(f, "  No assets uploaded.")
        } else {
            writeln!(f, "  Uploaded assets:")?;
            for asset in &self.uploaded_assets {
                writeln!(f, "    - {}", asset)?;
            }
            Ok(())
        }
    }
}

impl PrettyPrintable for DraftReleaseResult {
    fn pretty_print(&self) -> String {
        self.to_string()
    }
}

/// Test-only re-export so the integration test module can call the private function.
#[cfg(test)]
pub(crate) async fn find_or_create_draft_release_for_test(
    octocrab: &octocrab::Octocrab,
    repo_releases: &octocrab::repos::releases::ReleasesHandler<'_, '_>,
    tag: &str,
    name: Option<&str>,
) -> anyhow::Result<octocrab::models::repos::Release> {
    find_or_create_draft_release(octocrab, repo_releases, tag, name).await
}

/// Finds an existing draft release by tag name, or creates a new one.
async fn find_or_create_draft_release(
    octocrab: &octocrab::Octocrab,
    repo_releases: &octocrab::repos::releases::ReleasesHandler<'_, '_>,
    tag: &str,
    name: Option<&str>,
) -> anyhow::Result<octocrab::models::repos::Release> {
    // get_by_tag only surfaces published releases; drafts always 404 there.
    match repo_releases.get_by_tag(tag).await {
        Ok(release) => {
            tracing::info!("Found published release id={} for tag {}", release.id, tag);
            if !release.draft {
                tracing::warn!(
                    "Release for tag {} is already published (not a draft). Assets may not be modifiable if immutable releases are enabled.",
                    tag
                );
            }
            Ok(release)
        }
        Err(octocrab::Error::GitHub { source, .. })
            if source.status_code == http::StatusCode::NOT_FOUND =>
        {
            tracing::info!("No published release for tag {}; searching drafts", tag);
            let first_page = repo_releases
                .list()
                .per_page(100)
                .send()
                .await
                .with_context(|| "Failed to list releases to find draft")?;
            let all_releases = octocrab
                .all_pages::<octocrab::models::repos::Release>(first_page)
                .await
                .with_context(|| "Failed to paginate releases to find draft")?;

            let existing = all_releases
                .into_iter()
                .find(|r| r.draft && r.tag_name == tag);

            match existing {
                Some(draft) => {
                    tracing::info!(
                        "Found existing draft release id={} for tag {}",
                        draft.id,
                        tag
                    );
                    Ok(draft)
                }
                None => {
                    tracing::info!("Creating new draft release for tag {}", tag);
                    let mut builder = repo_releases.create(tag).draft(true);
                    if let Some(n) = name {
                        builder = builder.name(n);
                    }
                    let release: octocrab::models::repos::Release =
                        builder.send().await.with_context(|| {
                            format!("Failed to create draft release for tag: {}", tag)
                        })?;
                    Ok(release)
                }
            }
        }
        Err(e) => Err(e).with_context(|| format!("Failed to fetch release for tag: {}", tag)),
    }
}

/// Validates the combination of options before any I/O is performed.
///
/// Returns an error describing the first violated constraint, if any.
pub fn validate_options(options: &Options) -> anyhow::Result<()> {
    if options.release_tag.is_none() && options.version.is_empty() {
        anyhow::bail!("--version or --release-tag is required");
    }

    if options.release_tag.is_none()
        && options.tag_format.contains("{package_name}")
        && options.package_name.is_empty()
    {
        anyhow::bail!("--package-name is required when --tag-format contains {{package_name}}");
    }

    Ok(())
}

/// Creates or updates a draft GitHub release and uploads all files from the artifacts directory.
pub async fn draft_release(
    options: Box<Options>,
    _working_directory: PathBuf,
) -> anyhow::Result<DraftReleaseResult> {
    let (Some(github_app_id), Some(github_app_private_key)) = (
        options.github_app_id,
        options.github_app_private_key.clone(),
    ) else {
        anyhow::bail!("--github-app-id and --github-app-private-key are required");
    };

    validate_options(&options)?;

    let tag = match &options.release_tag {
        Some(explicit) => explicit.clone(),
        None => format_tag(&options.tag_format, &options.package_name, &options.version),
    };

    tracing::info!("Targeting draft release for tag: {}", tag);

    let github_token = generate_github_app_token(
        github_app_id,
        github_app_private_key,
        InstallationRetrievalMode::Organization,
        Some(options.repo_owner.clone()),
    )
    .await?;

    let octocrab = Octocrab::builder().personal_token(github_token).build()?;
    let repo = octocrab.repos(&options.repo_owner, &options.repo_name);
    let repo_releases = repo.releases();

    let release = find_or_create_draft_release(
        &octocrab,
        &repo_releases,
        &tag,
        options.release_name.as_deref(),
    )
    .await?;

    let release_id = release.id.into_inner();
    let artifact_dir = &options.artifacts;

    let uploaded_assets = if artifact_dir.is_dir() {
        upload_artifacts_to_release(&repo, release_id, artifact_dir)
            .await
            .with_context(|| format!("Failed to upload artifacts to release {}", tag))?
    } else {
        tracing::warn!(
            "Artifact directory does not exist or is not a directory: {:?}",
            artifact_dir
        );
        Vec::new()
    };

    Ok(DraftReleaseResult {
        tag,
        release_id,
        uploaded_assets,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_options() -> Options {
        Options {
            repo_owner: "owner".to_string(),
            repo_name: "repo".to_string(),
            github_app_id: None,
            github_app_private_key: None,
            artifacts: PathBuf::from("."),
            release_tag: None,
            tag_format: "{package_name}-{version}".to_string(),
            package_name: String::new(),
            version: String::new(),
            release_name: None,
        }
    }

    #[test]
    fn should_pass_when_release_tag_provided_and_version_empty() {
        // Arrange
        let options = Options {
            release_tag: Some("v1.0.0".to_string()),
            version: String::new(),
            ..base_options()
        };

        // Act
        let result = validate_options(&options);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn should_error_when_release_tag_absent_and_version_empty() {
        // Arrange
        let options = Options {
            release_tag: None,
            version: String::new(),
            ..base_options()
        };

        // Act
        let result = validate_options(&options);

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("--version or --release-tag is required")
        );
    }

    #[test]
    fn should_error_when_tag_format_uses_package_name_and_package_name_empty() {
        // Arrange
        let options = Options {
            release_tag: None,
            tag_format: "{package_name}-{version}".to_string(),
            package_name: String::new(),
            version: "1.2.3".to_string(),
            ..base_options()
        };

        // Act
        let result = validate_options(&options);

        // Assert
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("--package-name is required")
        );
    }

    #[test]
    fn should_pass_when_tag_format_omits_package_name_placeholder() {
        // Arrange
        let options = Options {
            release_tag: None,
            tag_format: "v{version}".to_string(),
            package_name: String::new(),
            version: "1.2.3".to_string(),
            ..base_options()
        };

        // Act
        let result = validate_options(&options);

        // Assert
        assert!(result.is_ok());
    }

    #[test]
    fn should_pass_when_tag_format_uses_package_name_and_all_fields_provided() {
        // Arrange
        let options = Options {
            release_tag: None,
            tag_format: "{package_name}-{version}".to_string(),
            package_name: "my-crate".to_string(),
            version: "1.2.3".to_string(),
            ..base_options()
        };

        // Act
        let result = validate_options(&options);

        // Assert
        assert!(result.is_ok());
    }
}
