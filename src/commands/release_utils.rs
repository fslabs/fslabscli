use std::{
    fs::{self, File},
    io::Read,
    path::Path,
};

use anyhow::Context;

/// Interpolates a tag format template with the given package name and version.
/// Supported placeholders: `{package_name}`, `{version}`.
pub fn format_tag(template: &str, package_name: &str, version: &str) -> String {
    template
        .replace("{package_name}", package_name)
        .replace("{version}", version)
}

/// Uploads all files from `artifact_dir` to a GitHub release, replacing any
/// existing asset with the same name to avoid GitHub's 422 on duplicate uploads.
///
/// Returns the list of uploaded asset names.
pub async fn upload_artifacts_to_release(
    repo: &octocrab::repos::RepoHandler<'_>,
    release_id: u64,
    artifact_dir: &Path,
) -> anyhow::Result<Vec<String>> {
    let repo_releases = repo.releases();

    // Fetch existing assets once — GitHub returns 422 on duplicate uploads.
    let existing_assets = repo_releases
        .assets(release_id)
        .per_page(100)
        .send()
        .await
        .with_context(|| format!("Failed to list assets for release {}", release_id))?;

    let mut uploaded_assets: Vec<String> = Vec::new();

    let entries = fs::read_dir(artifact_dir)
        .with_context(|| format!("Failed to read artifact directory: {:?}", artifact_dir))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(asset_name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };

        tracing::debug!("Uploading artifact: {}", asset_name);

        let metadata = fs::metadata(&path).with_context(|| format!("Failed to stat {:?}", path))?;
        let mut file = File::open(&path).with_context(|| format!("Failed to open {:?}", path))?;
        let mut data: Vec<u8> = Vec::with_capacity(metadata.len() as usize);
        file.read_to_end(&mut data)
            .with_context(|| format!("Failed to read {:?}", path))?;

        for asset in &existing_assets.items {
            if asset.name == asset_name {
                repo.release_assets()
                    .delete(asset.id.into_inner())
                    .await
                    .with_context(|| format!("Failed to delete existing asset {}", asset.name))?;
                break;
            }
        }

        repo_releases
            .upload_asset(release_id, asset_name, data.into())
            .send()
            .await
            .with_context(|| format!("Failed to upload asset: {}", asset_name))?;

        tracing::info!("Uploaded: {}", asset_name);
        uploaded_assets.push(asset_name.to_string());
    }

    Ok(uploaded_assets)
}
