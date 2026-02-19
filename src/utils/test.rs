use std::{
    fs::{self, File, OpenOptions, create_dir_all},
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

fn git(repo_path: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(repo_path);
    cmd.env("GIT_AUTHOR_NAME", "Test User");
    cmd.env("GIT_AUTHOR_EMAIL", "test@example.com");
    cmd.env("GIT_COMMITTER_NAME", "Test User");
    cmd.env("GIT_COMMITTER_EMAIL", "test@example.com");
    cmd
}

pub fn init_repo(repo_path: &Path) {
    let output = git(repo_path)
        .arg("init")
        .output()
        .expect("Failed to run git init");
    assert!(output.status.success(), "git init failed: {:?}", output);

    let output = git(repo_path)
        .args(["config", "commit.gpgsign", "false"])
        .output()
        .expect("Failed to configure git");
    assert!(output.status.success());
}

pub fn commit_all_changes(repo_path: &Path, message: &str) {
    stage_all(repo_path);
    commit_repo(repo_path, message);
}

pub fn modify_file(repo_path: &Path, file_path: &str, content: &str) {
    let full_path = repo_path.join(file_path);

    // Ensure the directory exists
    create_dir_all(full_path.parent().unwrap()).expect("Failed to create directories");

    // Modify the file
    std::fs::write(&full_path, content).expect("Failed to write to file");
}

pub fn stage_file(repo_path: &Path, file_path: &str) {
    let output = git(repo_path)
        .args(["add", file_path])
        .output()
        .expect("Failed to run git add");
    assert!(output.status.success(), "git add failed: {:?}", output);
}

pub fn stage_all(repo_path: &Path) {
    let output = git(repo_path)
        .args(["add", "-A"])
        .output()
        .expect("Failed to run git add -A");
    assert!(output.status.success(), "git add -A failed: {:?}", output);
}

pub fn commit_repo(repo_path: &Path, commit_message: &str) {
    let output = git(repo_path)
        .args(["commit", "-m", commit_message, "--allow-empty"])
        .output()
        .expect("Failed to run git commit");
    assert!(output.status.success(), "git commit failed: {:?}", output);
}

pub static FAKE_REGISTRY: &str = "fake-registry";

pub fn initialize_workspace(
    base_path: &Path,
    workspace_name: &str,
    sub_crates: Vec<&str>,
    alt_registries: Vec<&str>,
    cargo_publish: bool,
) {
    // Create lib.rs and Cargo.toml
    let workspace_dir = base_path.join(workspace_name);
    create_dir_all(&workspace_dir).unwrap();
    Command::new("cargo")
        .arg("init")
        .arg("--lib")
        .arg("--name")
        .arg(workspace_name)
        .arg("--registry")
        .arg(FAKE_REGISTRY)
        .current_dir(&workspace_dir)
        .output()
        .expect("Failed to create simple crate");

    let config_toml_dir = base_path.join(".cargo");
    create_dir_all(&config_toml_dir).unwrap();
    let config_toml = config_toml_dir.join("config.toml");
    let config_toml_content = format!(
        "[registries.{FAKE_REGISTRY}]\nindex = \"ssh://git@ssh.shipyard.rs/{FAKE_REGISTRY}/crate-index.git\""
    );
    let mut file = File::create(config_toml).unwrap();
    writeln!(file, "{config_toml_content}").unwrap();

    if cargo_publish {
        // Set cargo publish
        let cargo_toml = workspace_dir.join("Cargo.toml");
        let toml_content = r#"
[package.metadata.fslabs.publish.cargo]
publish = true
"#;
        let mut file = OpenOptions::new().append(true).open(cargo_toml).unwrap();
        writeln!(file, "{toml_content}").unwrap();
    }

    if !alt_registries.is_empty() {
        // Set Alternate registry for crates_g
        let cargo_toml = workspace_dir.join("Cargo.toml");
        let toml_content = format!(
            "alternate_registries=[\"{}\"]",
            alt_registries.join("\", \"")
        );
        let mut file = OpenOptions::new().append(true).open(cargo_toml).unwrap();
        writeln!(file, "{toml_content}").unwrap();
    }

    if !sub_crates.is_empty() {
        let cargo_toml = base_path.join(workspace_name).join("Cargo.toml");
        let toml_content = "\n[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"".to_string();
        let mut file = OpenOptions::new().append(true).open(cargo_toml).unwrap();
        writeln!(file, "{toml_content}").unwrap();
        let sub_crates_dir = base_path.join(workspace_name).join("crates");
        for sub_crate in sub_crates {
            let sub_crate_dir = sub_crates_dir.join(sub_crate);
            create_dir_all(&sub_crate_dir).unwrap();
            Command::new("cargo")
                .arg("init")
                .arg("--lib")
                .arg("--name")
                .arg(format!("{workspace_name}__{sub_crate}"))
                .arg("--registry")
                .arg(FAKE_REGISTRY)
                .current_dir(&sub_crate_dir)
                .output()
                .expect("Failed to create simple crate");
        }
    }
}

pub fn create_complex_workspace(alt_registry: bool) -> PathBuf {
    let tmp = assert_fs::TempDir::new()
        .unwrap()
        .into_persistent()
        .to_path_buf();

    init_repo(&tmp);

    initialize_workspace(
        &tmp,
        "workspace_a",
        vec!["crates_a", "crates_b", "crates_c"],
        vec![],
        false,
    );

    initialize_workspace(
        &tmp,
        "workspace_d",
        vec!["crates_e", "crates_f"],
        vec![],
        false,
    );
    let alternate_registries = match alt_registry {
        true => vec!["some_other_registries"],
        false => vec![],
    };
    initialize_workspace(&tmp, "crates_g", vec![], alternate_registries, true);

    // Setup Deps
    // workspace_d/crates_e -> workspace_a/crates_a
    Command::new("cargo")
        .arg("add")
        .arg("--offline")
        .arg("--registry")
        .arg(FAKE_REGISTRY)
        .arg("--path")
        .arg("../../../workspace_a/crates/crates_a")
        .arg("workspace_a__crates_a")
        .current_dir(tmp.join("workspace_d").join("crates").join("crates_e"))
        .output()
        .expect("Failed to add workspace_a__crates_a to workspace_d__crates_e");
    // crates_g ->  workspace_d/crates_e
    Command::new("cargo")
        .arg("add")
        .arg("--offline")
        .arg("--registry")
        .arg(FAKE_REGISTRY)
        .arg("--path")
        .arg("../workspace_d/crates/crates_e")
        .arg("workspace_d__crates_e")
        .current_dir(tmp.join("crates_g"))
        .output()
        .expect("Failed to add workspace_d__crates_e");
    // crates_g ->  workspace_a/crates_b
    Command::new("cargo")
        .arg("add")
        .arg("--offline")
        .arg("--registry")
        .arg(FAKE_REGISTRY)
        .arg("--path")
        .arg("../workspace_a/crates/crates_b")
        .arg("workspace_a__crates_b")
        .current_dir(tmp.join("crates_g"))
        .output()
        .expect("Failed to add workspace_a__crates_b");
    // Create a rust-toolchain file
    modify_file(
        &tmp,
        "rust-toolchain.toml",
        "[toolchain]\nprofile = \"default\"\n channel = \"1.88\"",
    );
    // Stage and commit initial crate
    commit_all_changes(&tmp, "Initial commit");
    dunce::canonicalize(tmp).unwrap()
}

pub fn create_rust_index(checksum: &str) -> PathBuf {
    let tmp = assert_fs::TempDir::new()
        .unwrap()
        .into_persistent()
        .to_path_buf();

    init_repo(&tmp);

    // Create config.json
    let config_path = tmp.join("config.json");
    let mut config = File::create(config_path).unwrap();
    write!(config, r#"{{"dl":"https://example.com"}}"#).unwrap();

    // Create crate directory structure
    let crate_dir = tmp.join("cr/at");
    fs::create_dir_all(&crate_dir).unwrap();

    // Create crate index file with version entries
    let crate_file_path = crate_dir.join("crate-test");
    let mut crate_file = File::create(crate_file_path).unwrap();

    let entries = [
        format!(
            r#"{{"name":"crate-test","vers":"0.1.0","deps":[],"features":{{}},"cksum":"{}","yanked":false,"links":null}}"#,
            checksum
        ),
        format!(
            r#"{{"name":"crate-test","vers":"0.2.0","deps":[],"features":{{}},"cksum":"{}","yanked":false,"links":null}}"#,
            checksum
        ),
        format!(
            r#"{{"name":"crate-test","vers":"0.2.2","deps":[],"features":{{}},"cksum":"{}","yanked":false,"links":null}}"#,
            checksum
        ),
        format!(
            r#"{{"name":"crate-test","vers":"0.2.3","deps":[],"features":{{}},"cksum":"{}","yanked":true,"links":null}}"#,
            checksum
        ),
    ];

    for entry in entries {
        writeln!(crate_file, "{}", entry).unwrap();
    }

    commit_all_changes(&tmp, "Initial commit");
    dunce::canonicalize(tmp).unwrap()
}
