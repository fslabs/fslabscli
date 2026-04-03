use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Per-destination config (bucket-level settings)
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub struct S3Destination {
    #[serde(default)]
    pub bucket_name: Option<String>,
    #[serde(default)]
    pub bucket_region: Option<String>,
    #[serde(default)]
    pub bucket_prefix: Option<String>,
    #[serde(default)]
    pub cloudfront_distribution_id: Option<String>,
    #[serde(default)]
    pub sync_delete: Option<bool>,
    /// Env var prefix for credentials lookup. Defaults to "S3".
    #[serde(default)]
    pub credentials_env_prefix: Option<String>,
}

/// Top-level S3 publish config, backward-compatible with the single-destination format.
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "snake_case")]
pub struct PackageMetadataFslabsCiPublishS3 {
    #[serde(default)]
    pub publish: bool,
    #[serde(default)]
    pub build_command: String,
    #[serde(default)]
    pub output_dir: Option<String>,

    // Legacy single-destination fields (backward compat)
    #[serde(default)]
    pub bucket_name: Option<String>,
    #[serde(default)]
    pub bucket_region: Option<String>,
    #[serde(default)]
    pub bucket_prefix: Option<String>,
    #[serde(default)]
    pub cloudfront_distribution_id: Option<String>,
    #[serde(default)]
    pub sync_delete: Option<bool>,

    // Multi-destination map
    #[serde(default)]
    pub destinations: Option<HashMap<String, S3Destination>>,

    #[serde(default)]
    pub error: Option<String>,
}

impl PackageMetadataFslabsCiPublishS3 {
    /// Returns the resolved list of destinations.
    ///
    /// If `destinations` is set, uses that map. Otherwise synthesizes a single
    /// "default" destination from the top-level legacy fields.
    pub fn resolved_destinations(&self) -> HashMap<String, S3Destination> {
        if let Some(ref dests) = self.destinations {
            dests.clone()
        } else {
            let mut map = HashMap::new();
            map.insert(
                "default".to_string(),
                S3Destination {
                    bucket_name: self.bucket_name.clone(),
                    bucket_region: self.bucket_region.clone(),
                    bucket_prefix: self.bucket_prefix.clone(),
                    cloudfront_distribution_id: self.cloudfront_distribution_id.clone(),
                    sync_delete: self.sync_delete,
                    credentials_env_prefix: None,
                },
            );
            map
        }
    }

    pub async fn check(&mut self) -> anyhow::Result<()> {
        if !self.publish {
            return Ok(());
        }
        if self.destinations.is_none() {
            // Legacy format: require build_command + bucket fields
            let publish = !self.build_command.is_empty()
                && self.bucket_name.is_some()
                && self.bucket_region.is_some();
            self.publish = publish;
        } else {
            // Multi-destination: only require build_command
            let publish = !self.build_command.is_empty();
            self.publish = publish;
        }
        Ok(())
    }
}
