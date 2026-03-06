use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Parser;
use regex::Regex;
use serde::Serialize;
use toml_edit::DocumentMut;

use crate::PrettyPrintable;

#[derive(Debug, Parser)]
#[command(about = "Sync fdk_apps workspace into a downstream repo")]
pub struct Options {
    /// Path to the fdk_apps source directory
    #[arg(long)]
    source: PathBuf,
    /// Path to the target downstream repo
    #[arg(long)]
    target: PathBuf,
    /// Comma-separated list of app names to sync (defaults to all Cargo apps in source)
    #[arg(long, value_delimiter = ',')]
    apps: Vec<String>,
    /// Perform a dry run without making changes
    #[arg(long)]
    dry_run: bool,
    /// Create a PR after syncing
    #[arg(long)]
    create_pr: bool,
    /// Branch name for the PR
    #[arg(long)]
    branch_name: Option<String>,
    /// Base branch for the PR
    #[arg(long, default_value = "main")]
    base_branch: String,
}

#[derive(Serialize, Debug)]
pub struct SyncReposResult {
    synced_apps: Vec<String>,
    transformed_files: Vec<String>,
    dry_run: bool,
    pr_url: Option<String>,
}

impl Display for SyncReposResult {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Synced {} app(s)", self.synced_apps.len())?;
        if let Some(url) = &self.pr_url {
            write!(f, " — PR: {url}")?;
        }
        Ok(())
    }
}

impl PrettyPrintable for SyncReposResult {
    fn pretty_print(&self) -> String {
        let mut out = format!("Synced {} app(s):\n", self.synced_apps.len());
        for app in &self.synced_apps {
            out.push_str(&format!("  - {app}\n"));
        }
        if !self.transformed_files.is_empty() {
            out.push_str("\nTransformed files:\n");
            for file in &self.transformed_files {
                out.push_str(&format!("  - {file}\n"));
            }
        }
        if let Some(url) = &self.pr_url {
            out.push_str(&format!("\nPR: {url}\n"));
        }
        out
    }
}

/// Removes or rewrites `path` keys from all dependency tables in a Cargo.toml.
///
/// Operates on `[dependencies]`, `[dev-dependencies]`, `[build-dependencies]`,
/// and `[target.*.dependencies]` variants. All other fields are preserved.
///
/// For each dependency:
/// - If the dependency name is in `workspace_members`, the `path` is rewritten to
///   `../{name}` (sibling layout) and the `registry` key is removed so Cargo resolves
///   it locally rather than falling back to a registry.
/// - Otherwise the `path` key is stripped so Cargo resolves the dep via its registry.
pub fn strip_path_from_deps(toml_content: &str, workspace_members: &[String]) -> anyhow::Result<String> {
    let mut doc: DocumentMut = toml_content
        .parse()
        .context("Failed to parse TOML for strip_path_from_deps")?;

    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        if let Some(table) = doc.get_mut(section).and_then(|v| v.as_table_like_mut()) {
            for (key, item) in table.iter_mut() {
                if let Some(dep_table) = item.as_table_like_mut() {
                    if workspace_members.contains(&key.to_string()) {
                        // Workspace member: rewrite path for sibling layout, remove registry
                        dep_table.insert("path", toml_edit::value(format!("../{key}")));
                        dep_table.remove("registry");
                    } else {
                        dep_table.remove("path");
                    }
                }
            }
        }
    }

    // Handle [target.'cfg(...)'.dependencies] and similar platform-conditional sections
    if let Some(target_table) = doc.get_mut("target").and_then(|t| t.as_table_like_mut()) {
        for (_target_key, target_item) in target_table.iter_mut() {
            if let Some(target_cfg) = target_item.as_table_like_mut() {
                for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
                    if let Some(deps) = target_cfg.get_mut(section).and_then(|v| v.as_table_like_mut()) {
                        for (key, item) in deps.iter_mut() {
                            if let Some(dep_table) = item.as_table_like_mut() {
                                if workspace_members.contains(&key.to_string()) {
                                    // Workspace member: rewrite path for sibling layout, remove registry
                                    dep_table.insert("path", toml_edit::value(format!("../{key}")));
                                    dep_table.remove("registry");
                                } else {
                                    dep_table.remove("path");
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(doc.to_string())
}

/// Merges a source Cargo.toml into a destination Cargo.toml for downstream repo sync.
///
/// Rules applied:
/// - `[workspace.package.version]` is taken from source
/// - `[workspace.dependencies]` starts from source, then any dependencies present only in
///   destination are appended (preserving extras). For each source dep:
///   - If the dep name matches a workspace member (from `[workspace.members]`), its `path` is
///     rewritten to `apps/{name}` so it resolves locally in the target layout.
///   - Otherwise the `path` key is stripped so Cargo resolves via a registry.
/// - All `[patch.*]` sections are replaced with those from source
/// - `[workspace.members]`, `[workspace.default-members]`, and `[profile.*]` are left untouched
pub fn transform_root_cargo_toml(src_content: &str, dst_content: &str) -> anyhow::Result<String> {
    let src_doc: DocumentMut = src_content
        .parse()
        .context("Failed to parse source Cargo.toml")?;
    let mut dst_doc: DocumentMut = dst_content
        .parse()
        .context("Failed to parse destination Cargo.toml")?;

    // Extract workspace member names from the source (last path component of each member entry)
    // so we can distinguish local workspace crates from external dependencies.
    let src_members: Vec<String> = src_doc
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter_map(|s| s.rsplit('/').next())
                .map(|s| s.to_owned())
                .collect()
        })
        .unwrap_or_default();

    // Copy version from src workspace.package.version to dst
    if let Some(src_version) = src_doc
        .get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
    {
        if let Some(dst_pkg) = dst_doc
            .get_mut("workspace")
            .and_then(|w| w.as_table_like_mut())
            .and_then(|w| w.get_mut("package"))
            .and_then(|p| p.as_table_like_mut())
        {
            dst_pkg.insert("version", toml_edit::value(src_version));
        } else {
            tracing::warn!("Destination Cargo.toml has no [workspace.package] section, skipping version update");
        }
    }

    let src_deps_table = src_doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table_like());

    let src_dep_keys: Vec<String> = src_deps_table
        .map(|t| t.iter().map(|(k, _)| k.to_owned()).collect())
        .unwrap_or_default();

    // Collect destination-only entries before we mutate dst_doc
    let dst_only_entries: Vec<(String, toml_edit::Item)> = dst_doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table_like())
        .map(|t| {
            t.iter()
                .filter(|(k, _)| !src_dep_keys.contains(&k.to_string()))
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    // Replace workspace.dependencies in dst with src entries, rewriting paths for workspace
    // members and stripping paths for external deps.
    if let Some(src_deps) = src_doc
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table_like())
    {
        let mut new_deps = toml_edit::Table::new();
        for (key, item) in src_deps.iter() {
            let mut cloned = item.clone();
            if let Some(dep_table) = cloned.as_table_like_mut() {
                if src_members.contains(&key.to_string()) {
                    // Workspace member: rewrite path to the target layout
                    dep_table.insert("path", toml_edit::value(format!("apps/{key}")));
                } else {
                    // External dep: strip path so Cargo resolves via registry
                    dep_table.remove("path");
                }
            }
            new_deps.insert(key, cloned);
        }
        // Re-add destination-only extras
        for (key, item) in dst_only_entries {
            new_deps.insert(&key, item);
        }

        if let Some(ws) = dst_doc
            .get_mut("workspace")
            .and_then(|w| w.as_table_like_mut())
        {
            ws.insert("dependencies", toml_edit::Item::Table(new_deps));
        } else {
            tracing::warn!("Destination Cargo.toml has no [workspace] section, skipping dependencies merge");
        }
    }

    // Replace all [patch.*] sections: remove existing ones from dst, copy from src
    let src_patch_entries: Vec<(String, toml_edit::Item)> = src_doc
        .get("patch")
        .and_then(|p| p.as_table_like())
        .map(|t| {
            t.iter()
                .map(|(k, v)| (k.to_owned(), v.clone()))
                .collect()
        })
        .unwrap_or_default();

    dst_doc.remove("patch");

    if !src_patch_entries.is_empty() {
        let mut patch_table = toml_edit::Table::new();
        for (key, item) in src_patch_entries {
            patch_table.insert(&key, item);
        }
        dst_doc.insert("patch", toml_edit::Item::Table(patch_table));
    }

    Ok(dst_doc.to_string())
}

/// Ensures the FSL registry is present in a Cargo config.toml.
///
/// If `[registries.fsl]` already exists, it is left unchanged.
pub fn ensure_fsl_registry(config_content: &str) -> anyhow::Result<String> {
    const FSL_INDEX: &str = "sparse+https://crates.fsl.dev/api/v1/crates/";

    let mut doc: DocumentMut = config_content
        .parse()
        .context("Failed to parse Cargo config TOML")?;

    // Ensure [registries] exists
    if doc.get("registries").is_none() {
        doc.insert("registries", toml_edit::Item::Table(toml_edit::Table::new()));
    }

    let registries = doc
        .get_mut("registries")
        .and_then(|r| r.as_table_like_mut())
        .context("[registries] is not a table")?;

    // Only add fsl entry if it doesn't already exist
    if registries.get("fsl").is_none() {
        let mut fsl_table = toml_edit::Table::new();
        fsl_table.insert("index", toml_edit::value(FSL_INDEX));
        registries.insert("fsl", toml_edit::Item::Table(fsl_table));
    }

    Ok(doc.to_string())
}

/// Adjusts relative paths in a Rust source file that escape the app root directory.
///
/// When an app moves from `source/<app>/` to `target/apps/<app>/`, paths that reference
/// above the app root need one additional `../` to account for the extra nesting level.
///
/// `depth` is the file's depth from the app root (e.g., `src/app/window.rs` has depth 2).
pub fn adjust_relative_paths(content: &str, depth: usize) -> String {
    // Match quoted strings starting a relative path inside include macros.
    // Captures: (1) macro prefix up to opening quote, (2) the full ../... path.
    let re = Regex::new(r#"(include(?:_bytes|_str)?!\s*\(\s*")((\.\./)+(.*?))"#).unwrap();

    re.replace_all(content, |caps: &regex::Captures| {
        let prefix = &caps[1]; // e.g. `include_bytes!("`
        let full_path = &caps[2]; // e.g. `../../../../fdk/assets/...`

        // Count leading ../ sequences (each is exactly 3 chars, always at offset 0, 3, 6, …)
        let dotdot_count = full_path
            .match_indices("../")
            .take_while(|(i, _)| *i % 3 == 0)
            .count();

        if dotdot_count > depth {
            // Path escapes the app root — prepend one more ../
            format!("{prefix}../{full_path}")
        } else {
            // Path stays within the app — no change needed
            format!("{prefix}{full_path}")
        }
    })
    .to_string()
}

/// Walks all `.rs` files in a synced app directory and adjusts relative paths
/// that escape the app root.
async fn adjust_app_relative_paths(app_dir: &Path, dry_run: bool) -> anyhow::Result<Vec<String>> {
    let mut adjusted_files = Vec::new();

    for entry in walkdir::WalkDir::new(app_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let file_path = entry.path();
        let content = tokio::fs::read_to_string(file_path)
            .await
            .with_context(|| format!("Failed to read {}", file_path.display()))?;

        // Skip files that don't contain include macros with relative paths
        if !content.contains("include") || !content.contains("../") {
            continue;
        }

        // Depth = number of path components relative to app_dir, minus the filename itself
        let relative = file_path.strip_prefix(app_dir).unwrap_or(file_path);
        let depth = relative.components().count().saturating_sub(1);

        let adjusted = adjust_relative_paths(&content, depth);

        if adjusted != content {
            if dry_run {
                tracing::info!("[dry-run] Would adjust relative paths in {}", file_path.display());
            } else {
                tokio::fs::write(file_path, &adjusted)
                    .await
                    .with_context(|| format!("Failed to write {}", file_path.display()))?;
                tracing::info!("Adjusted relative paths in {}", file_path.display());
            }
            adjusted_files.push(file_path.to_string_lossy().into_owned());
        }
    }

    Ok(adjusted_files)
}

/// Removes stale app directories from the target before rsync.
async fn remove_stale_app_dirs(
    target: &Path,
    apps: &[String],
    dry_run: bool,
) -> anyhow::Result<()> {
    for app in apps {
        let app_dir = target.join("apps").join(app);
        if app_dir.exists() {
            if dry_run {
                tracing::info!("[dry-run] Would remove directory: {}", app_dir.display());
            } else {
                tokio::fs::remove_dir_all(&app_dir)
                    .await
                    .with_context(|| format!("Failed to remove directory: {}", app_dir.display()))?;
            }
        }
    }
    Ok(())
}

/// Syncs a single app directory from source to target via rsync.
async fn sync_app_dir(
    source: &Path,
    target: &Path,
    app: &str,
    dry_run: bool,
) -> anyhow::Result<()> {
    let src_path = source.join(app);
    let dst_dir = target.join("apps").join(app);

    // Ensure destination directory exists so rsync has a valid target
    if !dry_run {
        tokio::fs::create_dir_all(&dst_dir)
            .await
            .with_context(|| format!("Failed to create directory: {}", dst_dir.display()))?;
    }

    // Append trailing slash to src so rsync copies the contents, not the directory itself
    let src_with_slash = format!("{}/", src_path.display());
    let dst_with_slash = format!("{}/", dst_dir.display());

    let mut cmd = tokio::process::Command::new("rsync");
    cmd.arg("-a")
        .arg("--delete")
        .arg("--exclude=dist")
        .arg("--exclude=target")
        .arg("--exclude=node_modules");
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.arg(&src_with_slash).arg(&dst_with_slash);
    cmd.current_dir(source);

    let output = cmd.output().await
        .with_context(|| format!("Failed to spawn rsync for app '{app}'"))?;

    if !output.status.success() {
        bail!("rsync failed for app '{app}': {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// Syncs the infra directory from source to target via rsync.
async fn sync_infra_dir(source: &Path, target: &Path, dry_run: bool) -> anyhow::Result<()> {
    let src_path = format!("{}/", source.join("infra").display());
    let dst_path = format!("{}/", target.join("infra").display());

    let mut cmd = tokio::process::Command::new("rsync");
    cmd.arg("-a")
        .arg("--delete")
        .arg("--exclude=.terraform")
        .arg("--exclude=*.tfstate*")
        .arg("--exclude=terraform.tfvars");
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.arg(&src_path).arg(&dst_path);
    cmd.current_dir(source);

    let output = cmd.output().await
        .context("Failed to spawn rsync for infra")?;

    if !output.status.success() {
        bail!("rsync failed for infra: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// Syncs `.cargo/config.toml` from source to target and ensures the FSL registry is present.
async fn sync_cargo_config(source: &Path, target: &Path, dry_run: bool) -> anyhow::Result<()> {
    let src_config_path = source.join(".cargo/config.toml");
    let dst_config_path = target.join(".cargo/config.toml");

    // Ensure the destination .cargo directory exists before rsync
    if !dry_run {
        if let Some(parent) = dst_config_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create .cargo directory in target")?;
        }
    }

    let mut cmd = tokio::process::Command::new("rsync");
    cmd.arg("-a");
    if dry_run {
        cmd.arg("--dry-run");
    }
    cmd.arg(&src_config_path).arg(&dst_config_path);
    cmd.current_dir(source);

    let output = cmd.output().await
        .context("Failed to spawn rsync for .cargo/config.toml")?;

    if !output.status.success() {
        bail!("rsync failed for .cargo/config.toml: {}", String::from_utf8_lossy(&output.stderr));
    }

    // Apply FSL registry patch — skip when dry_run because the destination file may not exist
    if !dry_run {
        let content = tokio::fs::read_to_string(&dst_config_path)
            .await
            .with_context(|| format!("Failed to read {}", dst_config_path.display()))?;

        let patched = ensure_fsl_registry(&content)?;

        tokio::fs::write(&dst_config_path, patched)
            .await
            .with_context(|| format!("Failed to write {}", dst_config_path.display()))?;
    } else {
        tracing::info!("[dry-run] Would patch .cargo/config.toml with FSL registry");
    }

    Ok(())
}

/// Reads, transforms, and writes the root Cargo.toml from source into target.
async fn transform_and_write_root_cargo_toml(
    source: &Path,
    target: &Path,
    dry_run: bool,
) -> anyhow::Result<()> {
    let src_path = source.join("Cargo.toml");
    let dst_path = target.join("Cargo.toml");

    let src_content = tokio::fs::read_to_string(&src_path)
        .await
        .with_context(|| format!("Failed to read source Cargo.toml: {}", src_path.display()))?;

    let dst_content = tokio::fs::read_to_string(&dst_path)
        .await
        .with_context(|| format!("Failed to read target Cargo.toml: {}", dst_path.display()))?;

    let transformed = transform_root_cargo_toml(&src_content, &dst_content)?;

    if dry_run {
        tracing::info!("[dry-run] Would write transformed Cargo.toml to {}", dst_path.display());
    } else {
        tokio::fs::write(&dst_path, transformed)
            .await
            .with_context(|| format!("Failed to write Cargo.toml: {}", dst_path.display()))?;
    }

    Ok(())
}

/// Strips or rewrites `path` keys from all dependency sections of an app's Cargo.toml in place.
///
/// See [`strip_path_from_deps`] for the member-aware rewrite rules.
async fn strip_and_write_app_cargo_toml(path: &Path, workspace_members: &[String], dry_run: bool) -> anyhow::Result<()> {
    let content = tokio::fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let stripped = strip_path_from_deps(&content, workspace_members)?;

    if dry_run {
        tracing::info!("[dry-run] Would strip path deps from {}", path.display());
    } else {
        tokio::fs::write(path, stripped)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }

    Ok(())
}

/// Creates a branch, commits the specified paths, pushes, and opens a GitHub PR.
///
/// Returns the PR URL from `gh pr create` stdout.
async fn create_branch_and_pr(
    target: &Path,
    branch: &str,
    base: &str,
    paths_to_stage: &[String],
) -> anyhow::Result<String> {
    // Step 1: create and switch to the new branch
    let checkout_output = tokio::process::Command::new("git")
        .arg("checkout")
        .arg("-b")
        .arg(branch)
        .current_dir(target)
        .output()
        .await
        .context("Failed to spawn git checkout")?;

    if !checkout_output.status.success() {
        bail!(
            "git checkout -b failed: {}",
            String::from_utf8_lossy(&checkout_output.stderr)
        );
    }

    // Step 2: stage only the files that were actually synced
    let mut add_cmd = tokio::process::Command::new("git");
    add_cmd.arg("add").current_dir(target);
    for path in paths_to_stage {
        add_cmd.arg(path);
    }

    let add_output = add_cmd.output().await.context("Failed to spawn git add")?;

    if !add_output.status.success() {
        bail!(
            "git add failed: {}",
            String::from_utf8_lossy(&add_output.stderr)
        );
    }

    // Step 3: commit
    let commit_output = tokio::process::Command::new("git")
        .arg("commit")
        .arg("-m")
        .arg("chore: sync fdk_apps")
        .current_dir(target)
        .output()
        .await
        .context("Failed to spawn git commit")?;

    if !commit_output.status.success() {
        bail!(
            "git commit failed: {}",
            String::from_utf8_lossy(&commit_output.stderr)
        );
    }

    // Step 4: push the branch to origin
    let push_output = tokio::process::Command::new("git")
        .arg("push")
        .arg("-u")
        .arg("origin")
        .arg(branch)
        .current_dir(target)
        .output()
        .await
        .context("Failed to spawn git push")?;

    if !push_output.status.success() {
        bail!(
            "git push failed: {}",
            String::from_utf8_lossy(&push_output.stderr)
        );
    }

    // Step 5: open the PR via GitHub CLI
    let pr_output = tokio::process::Command::new("gh")
        .arg("pr")
        .arg("create")
        .arg("--base")
        .arg(base)
        .arg("--title")
        .arg("chore: sync fdk_apps")
        .arg("--body")
        .arg("Automated sync from fdk_apps workspace")
        .current_dir(target)
        .output()
        .await
        .context("Failed to spawn gh pr create")?;

    if !pr_output.status.success() {
        bail!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&pr_output.stderr)
        );
    }

    Ok(String::from_utf8_lossy(&pr_output.stdout).trim().to_string())
}

/// Reads the workspace package version from a Cargo.toml file.
fn read_workspace_version(cargo_toml_path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(cargo_toml_path)
        .with_context(|| format!("Failed to read {}", cargo_toml_path.display()))?;
    let doc: DocumentMut = content
        .parse()
        .with_context(|| format!("Failed to parse {}", cargo_toml_path.display()))?;
    doc.get("workspace")
        .and_then(|w| w.get("package"))
        .and_then(|p| p.get("version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_owned())
        .context("No [workspace.package.version] found in source Cargo.toml")
}

/// Discovers app directories in the source path.
///
/// An app directory is any direct subdirectory that contains a `Cargo.toml`.
fn discover_apps(source: &Path) -> anyhow::Result<Vec<String>> {
    let mut apps = Vec::new();
    let entries = std::fs::read_dir(source)
        .with_context(|| format!("Failed to read source directory: {}", source.display()))?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("Cargo.toml").exists() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                apps.push(name.to_owned());
            }
        }
    }
    apps.sort();
    tracing::info!("Discovered {} app(s) from source: {}", apps.len(), apps.join(", "));
    Ok(apps)
}

/// Orchestrates the full sync from fdk_apps source into a downstream repository.
pub async fn sync_repos(
    options: Box<Options>,
    working_directory: PathBuf,
) -> anyhow::Result<SyncReposResult> {
    let source = dunce::canonicalize(working_directory.join(&options.source))
        .with_context(|| format!("Source path does not exist: {}", options.source.display()))?;
    let target = dunce::canonicalize(working_directory.join(&options.target))
        .with_context(|| format!("Target path does not exist: {}", options.target.display()))?;

    let apps = if options.apps.is_empty() {
        discover_apps(&source)?
    } else {
        options.apps.clone()
    };

    remove_stale_app_dirs(&target, &apps, options.dry_run).await?;

    for app in &apps {
        tracing::info!("Syncing app: {app}");
        sync_app_dir(&source, &target, app, options.dry_run).await?;
    }

    let infra_src = source.join("infra");
    let infra_synced = infra_src.exists();
    if infra_synced {
        tracing::info!("Syncing infra directory");
        sync_infra_dir(&source, &target, options.dry_run).await?;
    } else {
        tracing::info!("No infra directory found in source, skipping");
    }

    let cargo_config_src = source.join(".cargo/config.toml");
    let cargo_config_synced = cargo_config_src.exists();
    if cargo_config_synced {
        tracing::info!("Syncing .cargo/config.toml");
        sync_cargo_config(&source, &target, options.dry_run).await?;
    } else {
        tracing::info!("No .cargo/config.toml found in source, skipping");
    }

    tracing::info!("Transforming root Cargo.toml");
    transform_and_write_root_cargo_toml(&source, &target, options.dry_run).await?;

    let mut transformed_files = vec![target.join("Cargo.toml").to_string_lossy().into_owned()];

    for app in &apps {
        let app_cargo = target.join("apps").join(app).join("Cargo.toml");
        if app_cargo.exists() {
            tracing::info!("Stripping path deps from {}", app_cargo.display());
            strip_and_write_app_cargo_toml(&app_cargo, &apps, options.dry_run).await?;
            transformed_files.push(app_cargo.to_string_lossy().into_owned());
        }
    }

    // Adjust relative paths in Rust source files to account for apps/ nesting
    for app in &apps {
        let app_dir = target.join("apps").join(app);
        let adjusted = adjust_app_relative_paths(&app_dir, options.dry_run).await?;
        transformed_files.extend(adjusted);
    }

    // Delete stale Cargo.lock and regenerate fresh
    let lock_path = target.join("Cargo.lock");
    if lock_path.exists() {
        if options.dry_run {
            tracing::info!("[dry-run] Would delete {}", lock_path.display());
        } else {
            tokio::fs::remove_file(&lock_path)
                .await
                .with_context(|| format!("Failed to delete {}", lock_path.display()))?;
            tracing::info!("Deleted stale Cargo.lock");
        }
    }

    if !options.dry_run {
        tracing::info!("Regenerating Cargo.lock");
        let lockfile_output = tokio::process::Command::new("cargo")
            .arg("generate-lockfile")
            .current_dir(&target)
            .output()
            .await
            .context("Failed to spawn cargo generate-lockfile")?;

        if !lockfile_output.status.success() {
            bail!(
                "cargo generate-lockfile failed: {}",
                String::from_utf8_lossy(&lockfile_output.stderr)
            );
        }
    }

    transformed_files.push(target.join("Cargo.lock").to_string_lossy().into_owned());

    let pr_url = if options.create_pr && !options.dry_run {
        let default_branch;
        let branch = match &options.branch_name {
            Some(name) => name.as_str(),
            None => {
                let version = read_workspace_version(&source.join("Cargo.toml"))?;
                default_branch = format!("chore/sync-fdk-apps-{version}");
                &default_branch
            }
        };
        tracing::info!("Creating PR on branch '{branch}' targeting '{}'", options.base_branch);

        // Build the list of paths that were actually modified so we stage only those
        let mut paths_to_stage: Vec<String> = apps
            .iter()
            .map(|app| format!("apps/{app}"))
            .collect();
        if infra_synced {
            paths_to_stage.push("infra/".to_owned());
        }
        if cargo_config_synced {
            paths_to_stage.push(".cargo/config.toml".to_owned());
        }
        paths_to_stage.push("Cargo.toml".to_owned());
        paths_to_stage.push("Cargo.lock".to_owned());

        Some(create_branch_and_pr(&target, branch, &options.base_branch, &paths_to_stage).await?)
    } else {
        None
    };

    Ok(SyncReposResult {
        synced_apps: apps,
        transformed_files,
        dry_run: options.dry_run,
        pr_url,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;

    #[test]
    fn test_strip_path_from_deps() {
        let input = indoc! {r#"
            [dependencies]
            serde = { version = "1.0", features = ["derive"] }
            my-crate = { path = "../my-crate", version = "0.1.0", registry = "fsl" }
            simple-crate = "2.0.0"

            [dev-dependencies]
            test-helper = { path = "../test-helper", version = "0.2.0" }

            [build-dependencies]
            build-tool = { path = "../build-tool", features = ["fancy"] }
        "#};

        let result = strip_path_from_deps(input, &[]).unwrap();

        // path keys must be gone
        assert!(!result.contains("path = "));
        // version, registry, features must survive
        assert!(result.contains(r#"version = "1.0""#));
        assert!(result.contains(r#"version = "0.1.0""#));
        assert!(result.contains(r#"registry = "fsl""#));
        assert!(result.contains(r#"features = ["derive"]"#));
        assert!(result.contains(r#"version = "0.2.0""#));
        assert!(result.contains(r#"features = ["fancy"]"#));
        assert!(result.contains(r#"simple-crate = "2.0.0""#));
    }

    #[test]
    fn test_strip_path_noop_when_no_paths() {
        let input = indoc! {r#"
            [dependencies]
            serde = { version = "1.0", features = ["derive"] }
            anyhow = "1.0"

            [dev-dependencies]
            mockall = "0.13"
        "#};

        let result = strip_path_from_deps(input, &[]).unwrap();

        // Round-trip through toml_edit may reformat slightly, so check key content
        assert!(result.contains("serde"));
        assert!(result.contains("anyhow"));
        assert!(result.contains("mockall"));
        assert!(!result.contains("path"));
    }

    #[test]
    fn test_strip_path_from_deps_rewrites_member_paths() {
        let input = indoc! {r#"
            [dependencies]
            spatial_drive = { path = "../spatial_drive", version = "*", registry = "fsl" }
            serde = { path = "../vendored/serde", version = "1.0" }
            clap = { version = "4.0" }
        "#};

        let members = vec!["spatial_drive".to_owned()];
        let result = strip_path_from_deps(input, &members).unwrap();

        // spatial_drive is a workspace member: path rewritten, registry removed
        assert!(result.contains(r#"path = "../spatial_drive""#));
        assert!(!result.contains(r#"registry = "fsl""#));
        // serde is NOT a member: path stripped
        assert!(result.contains("serde"));
        assert!(!result.contains("vendored"));
        // clap untouched
        assert!(result.contains("clap"));
    }

    #[test]
    fn test_transform_root_cargo_toml_version() {
        let src = indoc! {r#"
            [workspace.package]
            version = "2.0.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let dst = indoc! {r#"
            [workspace.package]
            version = "1.0.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let result = transform_root_cargo_toml(src, dst).unwrap();
        assert!(result.contains(r#"version = "2.0.0""#));
        assert!(!result.contains(r#"version = "1.0.0""#));
    }

    #[test]
    fn test_transform_root_cargo_toml_members_preserved() {
        let src = indoc! {r#"
            [workspace]
            members = ["apps/src-app"]

            [workspace.package]
            version = "1.0.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let dst = indoc! {r#"
            [workspace]
            members = ["apps/dst-app-a", "apps/dst-app-b"]
            default-members = ["apps/dst-app-a"]

            [workspace.package]
            version = "0.9.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let result = transform_root_cargo_toml(src, dst).unwrap();
        assert!(result.contains("dst-app-a"));
        assert!(result.contains("dst-app-b"));
        assert!(!result.contains("src-app"));
    }

    #[test]
    fn test_transform_root_cargo_toml_deps_merged() {
        let src = indoc! {r#"
            [workspace]
            members = ["dep-a", "dep-b"]

            [workspace.package]
            version = "1.0.0"

            [workspace.dependencies]
            dep-a = { path = "dep-a", version = "1.0" }
            dep-b = { path = "dep-b", version = "2.0" }
            serde = { path = "../vendored/serde", version = "1.0" }
        "#};

        let dst = indoc! {r#"
            [workspace.package]
            version = "0.9.0"

            [workspace.dependencies]
            dep-a = { version = "1.0" }
            dep-c = { version = "3.0", registry = "fsl" }
        "#};

        let result = transform_root_cargo_toml(src, dst).unwrap();

        // dep-a and dep-b from src — workspace members get path rewritten
        assert!(result.contains("dep-a"));
        assert!(result.contains("dep-b"));
        assert!(result.contains(r#"path = "apps/dep-a""#));
        assert!(result.contains(r#"path = "apps/dep-b""#));
        // serde is NOT a workspace member — path stripped
        assert!(result.contains("serde"));
        assert!(!result.contains("vendored"));
        // dep-c preserved from dst
        assert!(result.contains("dep-c"));
        assert!(result.contains(r#"registry = "fsl""#));
    }

    #[test]
    fn test_transform_root_cargo_toml_patches_from_source() {
        let src = indoc! {r#"
            [workspace.package]
            version = "1.0.0"

            [workspace.dependencies]
            serde = "1.0"

            [patch.crates-io]
            serde = { git = "https://github.com/example/serde" }
        "#};

        let dst = indoc! {r#"
            [workspace.package]
            version = "0.9.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let result = transform_root_cargo_toml(src, dst).unwrap();
        assert!(result.contains("patch"));
        assert!(result.contains("crates-io"));
        assert!(result.contains("https://github.com/example/serde"));
    }

    #[test]
    fn test_transform_root_cargo_toml_profiles_preserved() {
        let src = indoc! {r#"
            [workspace.package]
            version = "1.0.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let dst = indoc! {r#"
            [workspace.package]
            version = "0.9.0"

            [workspace.dependencies]
            serde = "1.0"

            [profile.release]
            opt-level = 3
            lto = true
            codegen-units = 1
        "#};

        let result = transform_root_cargo_toml(src, dst).unwrap();
        assert!(result.contains("[profile.release]"));
        assert!(result.contains("opt-level = 3"));
        assert!(result.contains("lto = true"));
        assert!(result.contains("codegen-units = 1"));
    }

    #[test]
    fn test_ensure_fsl_registry_adds_missing() {
        let input = indoc! {r#"
            [source.crates-io]
            replace-with = "vendored-sources"
        "#};

        let result = ensure_fsl_registry(input).unwrap();
        assert!(result.contains("fsl"));
        assert!(result.contains("sparse+https://crates.fsl.dev/api/v1/crates/"));
    }

    #[test]
    fn test_ensure_fsl_registry_preserves_existing() {
        let input = indoc! {r#"
            [registries.fsl]
            index = "sparse+https://custom.registry.example.com/"
        "#};

        let result = ensure_fsl_registry(input).unwrap();
        // Custom index must not be overwritten
        assert!(result.contains("sparse+https://custom.registry.example.com/"));
        assert!(!result.contains("sparse+https://crates.fsl.dev/api/v1/crates/"));
    }

    #[test]
    fn test_transform_root_cargo_toml_no_src_deps_preserves_dst() {
        let src = indoc! {r#"
            [workspace.package]
            version = "2.0.0"
        "#};

        let dst = indoc! {r#"
            [workspace.package]
            version = "1.0.0"

            [workspace.dependencies]
            serde = "1.0"
        "#};

        let result = transform_root_cargo_toml(src, dst).unwrap();
        // Version updated
        assert!(result.contains(r#"version = "2.0.0""#));
        // Deps preserved from dst since src has none
        assert!(result.contains("serde"));
    }

    #[test]
    fn test_discover_apps() {
        let dir = assert_fs::TempDir::new().unwrap();
        // Create app dirs with Cargo.toml
        std::fs::create_dir_all(dir.path().join("app_a")).unwrap();
        std::fs::write(dir.path().join("app_a/Cargo.toml"), "[package]\nname = \"app_a\"").unwrap();
        std::fs::create_dir_all(dir.path().join("app_b")).unwrap();
        std::fs::write(dir.path().join("app_b/Cargo.toml"), "[package]\nname = \"app_b\"").unwrap();
        // Create a dir WITHOUT Cargo.toml — should be excluded
        std::fs::create_dir_all(dir.path().join("not_an_app")).unwrap();
        // Create a file (not a dir) — should be excluded
        std::fs::write(dir.path().join("Cargo.toml"), "[workspace]").unwrap();

        let apps = discover_apps(dir.path()).unwrap();
        assert_eq!(apps, vec!["app_a", "app_b"]);
    }

    #[test]
    fn test_adjust_relative_paths_adds_level() {
        let input = indoc! {r#"
            let icon_buf = std::io::Cursor::new(include_bytes!(
                "../../../../fdk/assets/branding/SD_32_W.png"
            ));
        "#};

        // File at depth 2 (e.g., src/app/window.rs): 4 ../ > 2 depth → needs adjustment
        let result = adjust_relative_paths(input, 2);
        assert!(result.contains("../../../../../fdk/assets/branding/SD_32_W.png"));
    }

    #[test]
    fn test_adjust_relative_paths_no_change_within_app() {
        let input = indoc! {r#"
            let data = include_bytes!("../data/test.bin");
        "#};

        // File at depth 1 (e.g., src/main.rs): 1 ../ <= 1 depth → no change
        let result = adjust_relative_paths(input, 1);
        assert!(result.contains("\"../data/test.bin\""));
        assert!(!result.contains("../../"));
    }

    #[test]
    fn test_adjust_relative_paths_include_str() {
        let input = r#"let sql = include_str!("../../../schema.sql");"#;

        // File at depth 2: 3 ../ > 2 → needs adjustment
        let result = adjust_relative_paths(input, 2);
        assert!(result.contains("../../../../schema.sql"));
    }
}
