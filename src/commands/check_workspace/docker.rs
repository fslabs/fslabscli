use crate::utils::docker::{Docker, RealHttpClient, RealOciClient};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Clone, Default, Debug)]
pub struct PackageMetadataFslabsCiPublishDocker {
    pub publish: bool,
    pub repository: Option<String>,
    pub context: Option<String>,
    pub dockerfile: Option<String>,
    /// BuildKit secret IDs mapped to environment variable names, never values.
    #[serde(default, deserialize_with = "deserialize_secrets")]
    pub secrets: BTreeMap<String, String>,
    #[serde(default)]
    pub error: Option<String>,
}

impl PackageMetadataFslabsCiPublishDocker {
    pub async fn check(
        &mut self,
        package: String,
        version: String,
        docker: &mut Docker<RealOciClient, RealHttpClient>,
    ) -> anyhow::Result<()> {
        if !self.publish {
            return Ok(());
        }
        let docker_registry = match self.repository.clone() {
            Some(r) => r,
            None => anyhow::bail!("Tried to check docker image without setting the registry"),
        };
        self.publish = !docker
            .check_image_exists(docker_registry, package, version)
            .await?;
        Ok(())
    }
}

fn deserialize_secrets<'de, D>(deserializer: D) -> Result<BTreeMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let secrets = BTreeMap::<String, String>::deserialize(deserializer)?;
    for (id, variable) in &secrets {
        let valid_id = !id.is_empty()
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c));
        let valid_variable = variable
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            && variable
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_');
        if !valid_id || !valid_variable {
            return Err(serde::de::Error::custom(
                "Docker secrets require an alphanumeric ID and an environment variable name",
            ));
        }
    }
    Ok(secrets)
}

#[cfg(test)]
mod tests {
    use super::PackageMetadataFslabsCiPublishDocker;

    #[test]
    fn accepts_environment_secret_mapping() {
        let metadata: PackageMetadataFslabsCiPublishDocker =
            serde_json::from_str(r#"{"publish":true,"secrets":{"source_token":"SOURCE_TOKEN"}}"#)
                .unwrap();
        assert_eq!(metadata.secrets["source_token"], "SOURCE_TOKEN");
    }

    #[test]
    fn existing_metadata_needs_no_secrets() {
        let metadata: PackageMetadataFslabsCiPublishDocker =
            serde_json::from_str(r#"{"publish":true}"#).unwrap();
        assert!(metadata.secrets.is_empty());
    }

    #[test]
    fn rejects_shell_syntax_and_literal_values() {
        for secrets in [
            serde_json::json!({"token;command": "SOURCE_TOKEN"}),
            serde_json::json!({"token": "$(command)"}),
            serde_json::json!({"token": "a-token-value"}),
            serde_json::json!({"token": ""}),
        ] {
            assert!(
                serde_json::from_value::<PackageMetadataFslabsCiPublishDocker>(
                    serde_json::json!({"publish": true, "secrets": secrets})
                )
                .is_err()
            );
        }
    }
}
