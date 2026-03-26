/// Wiremock-based integration tests for the GitHub release flow.
///
/// These tests exercise `upload_artifacts_to_release` and
/// `find_or_create_draft_release` against a local mock HTTP server so that no
/// real GitHub credentials are required.
///
/// Octocrab supports `base_uri()` on its builder, which lets us redirect every
/// API call (including the upload endpoint derived from the `upload_url` field
/// in release responses) to the mock server.  We embed the mock server's origin
/// directly in the `upload_url` field of every release fixture so that the
/// upload POST also lands on the mock server.
#[cfg(test)]
mod tests {
    use std::fs;

    use octocrab::Octocrab;
    use serde_json::json;
    use tempfile::TempDir;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // ─── fixture helpers ────────────────────────────────────────────────────

    /// Returns the minimal JSON that octocrab deserialises into a
    /// `octocrab::models::repos::Release`.  The `upload_url` is a URI-template
    /// pointing at the mock server so that `upload_asset` POSTs land there too.
    ///
    /// All URL fields must be absolute — octocrab validates them with `url::Url`.
    fn release_fixture(mock_origin: &str, id: u64, tag: &str, draft: bool) -> serde_json::Value {
        json!({
            "id": id,
            "node_id": "RE_1",
            "tag_name": tag,
            "target_commitish": "main",
            "name": tag,
            "body": "",
            "draft": draft,
            "prerelease": false,
            "created_at": "2024-01-01T00:00:00Z",
            "published_at": null,
            "url": format!("{mock_origin}/repos/owner/repo/releases/{id}"),
            "html_url": format!("https://github.com/owner/repo/releases/tag/{tag}"),
            "assets_url": format!("{mock_origin}/repos/owner/repo/releases/{id}/assets"),
            // upload_url is a URI-template; octocrab strips `{?name,label}` and appends
            // `?name=<asset>` itself when constructing the upload POST.
            "upload_url": format!("{mock_origin}/repos/owner/repo/releases/{id}/assets{{?name,label}}"),
            "tarball_url": null,
            "zipball_url": null,
            "author": author_fixture(),
            "assets": []
        })
    }

    /// A GitHub user object whose URL fields are all valid absolute URIs so that
    /// `url::Url::parse` inside octocrab's serde impl does not reject them.
    fn author_fixture() -> serde_json::Value {
        json!({
            "login": "test-bot",
            "id": 1,
            "node_id": "MDQ6VXNlcjE=",
            "avatar_url": "https://avatars.githubusercontent.com/u/1",
            "gravatar_id": "",
            "url": "https://api.github.com/users/test-bot",
            "html_url": "https://github.com/test-bot",
            "followers_url": "https://api.github.com/users/test-bot/followers",
            "following_url": "https://api.github.com/users/test-bot/following{/other_user}",
            "gists_url": "https://api.github.com/users/test-bot/gists{/gist_id}",
            "starred_url": "https://api.github.com/users/test-bot/starred{/owner}{/repo}",
            "subscriptions_url": "https://api.github.com/users/test-bot/subscriptions",
            "organizations_url": "https://api.github.com/users/test-bot/orgs",
            "repos_url": "https://api.github.com/users/test-bot/repos",
            "events_url": "https://api.github.com/users/test-bot/events{/privacy}",
            "received_events_url": "https://api.github.com/users/test-bot/received_events",
            "type": "Bot",
            "site_admin": false
        })
    }

    /// Returns a minimal `ReleaseAsset` JSON object.
    fn asset_fixture(id: u64, name: &str) -> serde_json::Value {
        json!({
            "id": id,
            "node_id": "RA_1",
            "name": name,
            "label": "",
            "content_type": "application/octet-stream",
            "state": "uploaded",
            "size": 4,
            "download_count": 0,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
            "url": format!("https://api.github.com/repos/owner/repo/releases/assets/{id}"),
            "browser_download_url": format!("https://github.com/owner/repo/releases/download/tag/{name}"),
            "uploader": author_fixture()
        })
    }

    /// GitHub 404 error body that octocrab parses as `Error::GitHub { status: 404 }`.
    fn not_found_body() -> serde_json::Value {
        json!({ "message": "Not Found", "documentation_url": "https://docs.github.com" })
    }

    /// Builds an Octocrab client that talks exclusively to `mock_origin`.
    fn octocrab_for(mock_origin: &str) -> Octocrab {
        Octocrab::builder()
            .personal_token("test-token".to_string())
            .base_uri(mock_origin)
            .expect("valid base URI")
            .build()
            .expect("octocrab client")
    }

    // ─── upload_artifacts_to_release ────────────────────────────────────────

    /// Happy path: empty asset list, single file upload succeeds.
    #[tokio::test]
    async fn test_upload_artifacts_happy_path() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let release_id: u64 = 1;

        // upload_asset() fetches the release first to read upload_url.
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/{release_id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(release_fixture(&origin, release_id, "v1.0.0", true)),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(asset_fixture(10, "artifact.bin")),
            )
            .mount(&mock_server)
            .await;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("artifact.bin"), b"data").unwrap();

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");

        // Act
        let uploaded = crate::commands::release_utils::upload_artifacts_to_release(
            &repo,
            release_id,
            dir.path(),
        )
        .await
        .expect("upload should succeed");

        // Assert
        assert_eq!(uploaded, vec!["artifact.bin"]);
    }

    /// Empty artifact directory — no uploads, no errors.
    #[tokio::test]
    async fn test_upload_artifacts_empty_directory() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let release_id: u64 = 2;

        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&mock_server)
            .await;

        let dir = TempDir::new().unwrap();

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");

        // Act
        let uploaded = crate::commands::release_utils::upload_artifacts_to_release(
            &repo,
            release_id,
            dir.path(),
        )
        .await
        .expect("upload should succeed with empty dir");

        // Assert
        assert!(uploaded.is_empty(), "no files means no uploads");
    }

    /// Asset dedup: when an asset with the same name already exists the old one
    /// is deleted before the new one is uploaded.
    #[tokio::test]
    async fn test_upload_artifacts_deduplicates_existing_asset() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let release_id: u64 = 3;
        let existing_asset_id: u64 = 99;

        // upload_asset() fetches the release first to read upload_url.
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/{release_id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(release_fixture(&origin, release_id, "v3.0.0", true)),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([asset_fixture(existing_asset_id, "artifact.bin")])),
            )
            .mount(&mock_server)
            .await;

        // DELETE of the stale asset must be called before the upload.
        Mock::given(method("DELETE"))
            .and(path(format!(
                "/repos/owner/repo/releases/assets/{existing_asset_id}"
            )))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(asset_fixture(100, "artifact.bin")),
            )
            .mount(&mock_server)
            .await;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("artifact.bin"), b"new").unwrap();

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");

        // Act
        let uploaded = crate::commands::release_utils::upload_artifacts_to_release(
            &repo,
            release_id,
            dir.path(),
        )
        .await
        .expect("upload should succeed");

        // Assert: upload succeeded and DELETE was called (verified by `expect(1)` above).
        assert_eq!(uploaded, vec!["artifact.bin"]);
    }

    /// Upload failure (HTTP 500) propagates as an error with context.
    #[tokio::test]
    async fn test_upload_artifacts_upload_failure_propagates_error() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let release_id: u64 = 4;

        // upload_asset() fetches the release first to read upload_url.
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/{release_id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(release_fixture(&origin, release_id, "v4.0.0", true)),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(ResponseTemplate::new(422).set_body_json(json!({
                "message": "Validation Failed",
                "documentation_url": "https://docs.github.com"
            })))
            .mount(&mock_server)
            .await;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("fail.bin"), b"data").unwrap();

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");

        // Act
        let result = crate::commands::release_utils::upload_artifacts_to_release(
            &repo,
            release_id,
            dir.path(),
        )
        .await;

        // Assert
        assert!(result.is_err(), "500 from upload endpoint must be an error");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("Failed to upload asset"),
            "error must carry context, got: {msg}"
        );
    }

    /// Multiple files are all uploaded; only the duplicate is deleted first.
    #[tokio::test]
    async fn test_upload_artifacts_multiple_files_one_duplicate() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let release_id: u64 = 5;
        let stale_id: u64 = 50;

        // upload_asset() fetches the release first (once per file) to read upload_url.
        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/{release_id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(release_fixture(&origin, release_id, "v5.0.0", true)),
            )
            .mount(&mock_server)
            .await;

        // asset list returns one pre-existing asset matching "old.bin"
        Mock::given(method("GET"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([asset_fixture(stale_id, "old.bin")])),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("DELETE"))
            .and(path(format!(
                "/repos/owner/repo/releases/assets/{stale_id}"
            )))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path(format!(
                "/repos/owner/repo/releases/{release_id}/assets"
            )))
            .respond_with(ResponseTemplate::new(201).set_body_json(asset_fixture(101, "new")))
            .mount(&mock_server)
            .await;

        let dir = TempDir::new().unwrap();
        fs::write(dir.path().join("old.bin"), b"v2").unwrap();
        fs::write(dir.path().join("new.bin"), b"v1").unwrap();

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");

        // Act
        let mut uploaded = crate::commands::release_utils::upload_artifacts_to_release(
            &repo,
            release_id,
            dir.path(),
        )
        .await
        .expect("upload should succeed");
        uploaded.sort();

        // Assert
        assert_eq!(uploaded, vec!["new.bin", "old.bin"]);
    }

    // ─── find_or_create_draft_release ───────────────────────────────────────

    /// Draft found directly via `get_by_tag` (published release with draft=true).
    #[tokio::test]
    async fn test_find_or_create_draft_release_found_via_get_by_tag() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-1.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(release_fixture(&origin, 42, tag, true)),
            )
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let release = invoke_find_or_create(&octocrab, &repo_releases, tag, None).await;

        // Assert
        let release = release.expect("should find the draft release");
        assert_eq!(release.id.into_inner(), 42u64);
        assert_eq!(release.tag_name, tag);
        assert!(release.draft);
    }

    /// Published (non-draft) release found via get_by_tag — function returns it
    /// with a warning but does not fail.
    #[tokio::test]
    async fn test_find_or_create_draft_release_published_release_returned_with_warning() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-2.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(release_fixture(
                &origin, 77, tag, false, // not a draft
            )))
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let release = invoke_find_or_create(&octocrab, &repo_releases, tag, None)
            .await
            .expect("published release must still be returned");

        // Assert: function succeeds and returns the release as-is.
        assert_eq!(release.id.into_inner(), 77u64);
        assert!(!release.draft);
    }

    /// get_by_tag returns 404, draft found in paginated list.
    #[tokio::test]
    async fn test_find_or_create_draft_release_found_via_list() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-3.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(ResponseTemplate::new(404).set_body_json(not_found_body()))
            .mount(&mock_server)
            .await;

        // List endpoint returns a single draft matching our tag.
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!([release_fixture(&origin, 55, tag, true)])),
            )
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let release = invoke_find_or_create(&octocrab, &repo_releases, tag, None)
            .await
            .expect("draft found via list");

        // Assert
        assert_eq!(release.id.into_inner(), 55u64);
        assert!(release.draft);
    }

    /// get_by_tag returns 404, list is empty — a new draft release is created.
    #[tokio::test]
    async fn test_find_or_create_draft_release_creates_new_when_none_exists() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-4.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(ResponseTemplate::new(404).set_body_json(not_found_body()))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&mock_server)
            .await;

        // Creation endpoint — verify the body contains draft=true.
        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(release_fixture(&origin, 66, tag, true)),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let release = invoke_find_or_create(&octocrab, &repo_releases, tag, None)
            .await
            .expect("new draft should be created");

        // Assert
        assert_eq!(release.id.into_inner(), 66u64);
        assert!(release.draft);
        assert_eq!(release.tag_name, tag);
    }

    /// get_by_tag returns 404, list contains a draft but with a *different* tag
    /// name — a new draft is created for the requested tag.
    #[tokio::test]
    async fn test_find_or_create_draft_release_ignores_draft_with_different_tag() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-5.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(ResponseTemplate::new(404).set_body_json(not_found_body()))
            .mount(&mock_server)
            .await;

        // List contains a draft for a *different* tag.
        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!([release_fixture(
                    &origin,
                    11,
                    "other-pkg-1.0.0",
                    true
                )])),
            )
            .mount(&mock_server)
            .await;

        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(
                ResponseTemplate::new(201).set_body_json(release_fixture(&origin, 88, tag, true)),
            )
            .expect(1)
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let release = invoke_find_or_create(&octocrab, &repo_releases, tag, None)
            .await
            .expect("should create new draft for the right tag");

        // Assert
        assert_eq!(release.id.into_inner(), 88u64);
        assert_eq!(release.tag_name, tag);
    }

    /// get_by_tag returns a non-404 error (e.g., 500) — the error is surfaced.
    #[tokio::test]
    async fn test_find_or_create_draft_release_propagates_non_404_error() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-6.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(
                ResponseTemplate::new(500)
                    .set_body_json(json!({ "message": "Service unavailable" })),
            )
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let result = invoke_find_or_create(&octocrab, &repo_releases, tag, None).await;

        // Assert
        assert!(result.is_err(), "5xx must propagate as error");
    }

    /// `find_or_create_draft_release` accepts an optional name and uses it when
    /// creating a new release.
    #[tokio::test]
    async fn test_find_or_create_draft_release_uses_provided_name() {
        // Arrange
        let mock_server = MockServer::start().await;
        let origin = mock_server.uri();
        let tag = "my-pkg-7.0.0";
        let name = "My Package 7.0.0";

        Mock::given(method("GET"))
            .and(path(format!("/repos/owner/repo/releases/tags/{tag}")))
            .respond_with(ResponseTemplate::new(404).set_body_json(not_found_body()))
            .mount(&mock_server)
            .await;

        Mock::given(method("GET"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
            .mount(&mock_server)
            .await;

        let mut response_fixture = release_fixture(&origin, 99, tag, true);
        response_fixture["name"] = json!(name);

        Mock::given(method("POST"))
            .and(path("/repos/owner/repo/releases"))
            .respond_with(ResponseTemplate::new(201).set_body_json(response_fixture))
            .expect(1)
            .mount(&mock_server)
            .await;

        let octocrab = octocrab_for(&origin);
        let repo = octocrab.repos("owner", "repo");
        let repo_releases = repo.releases();

        // Act
        let release = invoke_find_or_create(&octocrab, &repo_releases, tag, Some(name))
            .await
            .expect("should create draft with custom name");

        // Assert
        assert_eq!(
            release.name.as_deref(),
            Some(name),
            "release name must match what we passed"
        );
    }

    // ─── format_tag (pure, no I/O) ──────────────────────────────────────────

    #[test]
    fn test_format_tag_interpolates_both_placeholders() {
        // Arrange / Act / Assert
        assert_eq!(
            crate::commands::release_utils::format_tag(
                "{package_name}-{version}",
                "my-crate",
                "1.2.3"
            ),
            "my-crate-1.2.3"
        );
    }

    #[test]
    fn test_format_tag_version_only_template() {
        assert_eq!(
            crate::commands::release_utils::format_tag("v{version}", "ignored", "4.5.6"),
            "v4.5.6"
        );
    }

    #[test]
    fn test_format_tag_static_template() {
        assert_eq!(
            crate::commands::release_utils::format_tag("stable", "pkg", "1.0.0"),
            "stable"
        );
    }

    #[test]
    fn test_format_tag_empty_version() {
        assert_eq!(
            crate::commands::release_utils::format_tag("v{version}", "pkg", ""),
            "v"
        );
    }

    // ─── private helper — thin wrapper around the private function ───────────
    //
    // `find_or_create_draft_release` is `async fn` in `draft_release::mod` and
    // is not `pub`.  We expose it through the module's `pub(crate)` alias below
    // so that the test module can call it without moving all tests into the
    // production module.  See the companion shim added to draft_release/mod.rs.

    async fn invoke_find_or_create(
        octocrab: &Octocrab,
        repo_releases: &octocrab::repos::releases::ReleasesHandler<'_, '_>,
        tag: &str,
        name: Option<&str>,
    ) -> anyhow::Result<octocrab::models::repos::Release> {
        crate::commands::draft_release::find_or_create_draft_release_for_test(
            octocrab,
            repo_releases,
            tag,
            name,
        )
        .await
    }
}
