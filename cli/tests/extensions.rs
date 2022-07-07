#![cfg(feature = "extensions")]

use std::convert::TryFrom;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use assert_cmd::Command;
use lazy_static::lazy_static;
use phylum_cli::commands::extensions::extension::Extension;
use predicates::prelude::*;
use regex::Regex;
use tempfile::TempDir;

#[path = "extensions/module_loader.rs"]
mod module_loader;
#[path = "extensions/permissions.rs"]
mod permissions;

lazy_static! {
    // Lock this mutex when setting an environment variable, for the lifetime of function calls
    // depending on that specific environment variable value. Currently only used by the
    // `extension_is_installed_correctly` test. This trades some contention for the possibility
    // of running tests in parallel.
    static ref ENV_MUTEX: Mutex<()> = Mutex::new(());
}

////////////////////////////////////////////////////////////////////////////////
// Acceptance criteria tests
////////////////////////////////////////////////////////////////////////////////

// When a user runs `phylum extension add .`, the extension in the current
// working directory should be installed.
#[test]
fn extension_is_installed_correctly() {
    let tempdir = TempDir::new().unwrap();
    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("sample"))
        .assert()
        .success();

    let _guard = ENV_MUTEX.lock().unwrap();
    env::set_var("XDG_DATA_HOME", tempdir.path());

    let installed_ext = Extension::load("sample").unwrap();
    assert_eq!(installed_ext.name(), "sample");

    let not_installed_ext = Extension::load("sample-other");
    assert!(not_installed_ext.is_err());
}

// After a user installs a new extension, foobar, it should become available to
// the user under the phylum cli, e.g., running `phylum foobar` should execute
// the foobar extension.
#[test]
fn can_run_installed_extension() {
    let tempdir = TempDir::new().unwrap();
    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("sample"))
        .assert()
        .success();

    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("sample")
        .assert()
        .success()
        .stdout("Hello, World!\n");
}

// When a user installs a valid extension it should print a message indicating
// success. It should also print a quick guide on the extension to give the user
// some context on how the given extension works.
#[test]
fn successful_installation_prints_message() {
    let tempdir = TempDir::new().unwrap();
    let cmd = Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("sample"))
        .assert()
        .success();

    let output = std::str::from_utf8(&cmd.get_output().stdout).unwrap();
    assert!(output.lines().any(|m| m.contains("Extension sample installed successfully")));
}

// When a user attempts to install an invalid extension, it should fail and
// inform the user as to why.
#[test]
fn unsuccessful_installation_prints_failure_message() {
    let tempdir = TempDir::new().unwrap();

    fn stderr_match_regex(cmd: assert_cmd::assert::Assert, pattern: &str) -> bool {
        let output = std::str::from_utf8(&cmd.get_output().stderr).unwrap();

        output.lines().any(|m| m.contains(pattern))
    }

    // Install the extension. Should succeed.
    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("sample"))
        .assert()
        .success();

    // Reinstall the same extension. Should fail with an error.
    assert!(stderr_match_regex(
        Command::cargo_bin("phylum")
            .unwrap()
            .env("XDG_DATA_HOME", tempdir.path())
            .arg("extension")
            .arg("install")
            .arg(fixtures_path().join("sample"))
            .assert()
            .failure(),
        r#"extension already exists"#,
    ));

    // Try to install the extension from the installed path. Should fail with an
    // error.
    assert!(stderr_match_regex(
        Command::cargo_bin("phylum")
            .unwrap()
            .env("XDG_DATA_HOME", tempdir.path())
            .arg("extension")
            .arg("install")
            .arg(PathBuf::from(tempdir.path()).join("phylum").join("extensions").join("sample"),)
            .assert()
            .failure(),
        "skipping",
    ));
}

// When a user runs `phylum extension remove <extensionName>` the extension
// should be entirely removed from the user system.
#[test]
fn extension_is_uninstalled_correctly() {
    let tempdir = TempDir::new().unwrap();

    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("sample"))
        .assert()
        .success();

    let extension_path =
        tempdir.path().to_path_buf().join("phylum").join("extensions").join("sample");

    assert!(walkdir::WalkDir::new(&extension_path).into_iter().count() > 1);

    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("uninstall")
        .arg("sample")
        .assert()
        .success();

    assert!(!extension_path.exists());
}

// When a user runs phylum extension or phylum extension list a list of
// currently installed extensions, their versions and a short one sentence blurb
// on what the extension does should be shown in a table format.
#[test]
fn extension_list_should_emit_output() {
    let tempdir = TempDir::new().unwrap();

    // Output that no extension is installed when that is the case.
    let cmd = Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("list")
        .assert();

    let output = std::str::from_utf8(&cmd.get_output().stdout).unwrap();
    let re = Regex::new(r#"No extension"#).unwrap();

    assert!(output.lines().any(|m| re.is_match(m)));

    // Install one extension
    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("sample"))
        .assert();

    // Output name and description of the extension when one is installed
    let cmd = Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("list")
        .assert();

    let output = std::str::from_utf8(&cmd.get_output().stdout).unwrap();
    let re = Regex::new(r#"^sample\s+This extension does a thing"#).unwrap();

    assert!(output.lines().any(|m| re.is_match(m)));
}

// Extensions relying on the injected Phylum API work.
#[test]
fn injected_api() {
    let tempdir = TempDir::new().unwrap();
    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .args(&["extension", "install"])
        .arg(fixtures_path().join("api"))
        .assert()
        .success();

    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("api")
        .assert()
        .success()
        .stdout("44\n");
}

// Extensions can access CLI arguments.
#[test]
fn arg_access() {
    let tempdir = TempDir::new().unwrap();
    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("args"))
        .assert()
        .success();

    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .args(&["args", "--test", "-x", "a"])
        .assert()
        .success()
        .stdout("[ \"--test\", \"-x\", \"a\" ]\n");
}

// Extension creation works.
#[test]
fn create_extension() {
    let tempdir = TempDir::new().unwrap();
    Command::cargo_bin("phylum")
        .unwrap()
        .current_dir(tempdir.path())
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("new")
        .arg("my-ext")
        .assert()
        .success()
        .stderr(predicates::str::contains("✅ Extension created successfully"));
}

// Extension creation with invalid name fails
#[test]
fn create_incorrect_name() {
    let tempdir = TempDir::new().unwrap();
    Command::cargo_bin("phylum")
        .unwrap()
        .current_dir(tempdir.path())
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("new")
        .arg("@@@")
        .assert()
        .failure()
        .stderr(predicates::str::contains("invalid extension name"));
}

////////////////////////////////////////////////////////////////////////////////
// Miscellaneous tests
////////////////////////////////////////////////////////////////////////////////

#[test]
fn valid_extension_is_loaded_correctly() {
    let ext = Extension::try_from(fixtures_path().join("sample")).unwrap();

    assert_eq!(ext.name(), "sample");
}

#[test]
fn conflicting_extension_name_is_filtered() {
    let tempdir = TempDir::new().unwrap();

    Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("install")
        .arg(fixtures_path().join("ping"))
        .assert()
        .success();

    let cmd = Command::cargo_bin("phylum")
        .unwrap()
        .env("XDG_DATA_HOME", tempdir.path())
        .arg("extension")
        .arg("list")
        .assert()
        .success();

    let output = std::str::from_utf8(&cmd.get_output().stderr).unwrap();
    assert!(output.contains("extension was filtered out"));
}

////////////////////////////////////////////////////////////////////////////////
// Utilities
////////////////////////////////////////////////////////////////////////////////

fn project_root() -> PathBuf {
    Path::new(&env!("CARGO_MANIFEST_DIR")).ancestors().nth(1).unwrap().to_path_buf()
}

fn fixtures_path() -> PathBuf {
    project_root().join("cli").join("tests").join("fixtures").join("extensions")
}