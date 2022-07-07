use std::convert::TryFrom;
use std::fs::{self, DirBuilder};
#[cfg(unix)]
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

use anyhow::{anyhow, Result};
use futures::future::BoxFuture;
use lazy_static::lazy_static;
use log::{warn, LevelFilter};
use regex::Regex;
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::api::PhylumApi;
pub(crate) use crate::commands::extensions::api::ExtensionState;
use crate::commands::extensions::permissions::Permissions;
use crate::commands::{CommandResult, ExitCode};
use crate::{deno, dirs};

const MANIFEST_NAME: &str = "PhylumExt.toml";

lazy_static! {
    static ref EXTENSION_NAME_RE: Regex = Regex::new(r#"^[a-z][a-z0-9-]+$"#).unwrap();
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ExtensionManifest {
    name: String,
    description: Option<String>,
    entry_point: String,
    #[serde(default)]
    permissions: Permissions,
}

impl ExtensionManifest {
    pub fn new(
        name: String,
        entry_point: String,
        description: Option<String>,
        permissions: Option<Permissions>,
    ) -> Self {
        let permissions = permissions.unwrap_or_default();
        Self { description, entry_point, name, permissions }
    }
}

#[derive(Debug)]
pub struct Extension {
    path: PathBuf,
    manifest: ExtensionManifest,
}

impl Extension {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    pub fn description(&self) -> Option<&str> {
        self.manifest.description.as_deref()
    }

    pub fn entry_point(&self) -> &String {
        &self.manifest.entry_point
    }

    pub fn permissions(&self) -> &Permissions {
        &self.manifest.permissions
    }

    /// Install the extension in the default path.
    pub fn install(&self) -> Result<()> {
        println!("Installing extension {}...", self.name());

        let target_prefix = extension_path(self.name())?;

        // TODO we may want to implement `upgrade` in the future, which would
        // allow writing to the path of an already installed extension.
        if target_prefix.exists() {
            return Err(anyhow!("extension already exists, skipping"));
        }

        if target_prefix == self.path {
            return Err(anyhow!("extension path and installation path are identical, skipping"));
        }

        for entry in WalkDir::new(&self.path) {
            let source_path = entry?.into_path();
            let dest_path = target_prefix.join(source_path.strip_prefix(&self.path)?);

            if source_path.is_dir() {
                let mut builder = DirBuilder::new();

                #[cfg(unix)]
                builder.mode(0o700);

                builder.recursive(true);
                builder.create(&dest_path)?;
            } else if source_path.is_symlink() {
                warn!(
                    "install {}: `{:?}`: is a symbolic link, skipping",
                    self.manifest.name, source_path
                );
            } else if source_path.is_file() {
                if dest_path.exists() {
                    return Err(anyhow!("{}: already exists", dest_path.to_string_lossy()));
                } else {
                    fs::copy(source_path, dest_path)?;
                }
            }
        }

        println!("Extension {} installed successfully", self.name());

        Ok(())
    }

    pub fn uninstall(self) -> Result<()> {
        println!("Uninstalling extension {}...", self.name());
        let target_prefix = extension_path(self.name())?;

        if target_prefix != self.path {
            return Err(anyhow!("extension {} is not installed, skipping", self.name()));
        }

        fs::remove_dir_all(&self.path)?;

        println!("Extension {} uninstalled successfully", self.name());

        Ok(())
    }

    /// Load an extension from the default path.
    pub fn load(name: &str) -> Result<Extension, anyhow::Error> {
        Extension::try_from(extension_path(name)?)
    }

    /// Return the path to this extension's entry point.
    pub fn path(&self) -> PathBuf {
        self.path.join(&self.manifest.entry_point)
    }

    /// Execute an extension subcommand.
    pub async fn run(
        &self,
        api: BoxFuture<'static, Result<PhylumApi>>,
        args: Vec<String>,
    ) -> CommandResult {
        // Disable logging for running extensions.
        log::set_max_level(LevelFilter::Off);

        deno::run(ExtensionState::from(api), self, args).await?;

        Ok(ExitCode::Ok.into())
    }
}

// Load the extension from the specified path.
impl TryFrom<PathBuf> for Extension {
    type Error = anyhow::Error;

    fn try_from(path: PathBuf) -> Result<Self, Self::Error> {
        if !path.is_dir() {
            return Err(anyhow!("{}: not a directory", path.to_string_lossy()));
        }

        let manifest_path = path.join(MANIFEST_NAME);
        if !manifest_path.exists() {
            return Err(anyhow!("{}: missing {}", path.to_string_lossy(), MANIFEST_NAME));
        }

        let buf = fs::read(manifest_path)?;

        let manifest: ExtensionManifest = toml::from_slice(&buf)?;
        let entry_point_path = path.join(&manifest.entry_point);

        if !entry_point_path.exists() {
            return Err(anyhow!(
                "{}: entry point does not exist",
                entry_point_path.to_string_lossy()
            ));
        }

        if !entry_point_path.is_file() {
            return Err(anyhow!(
                "{}: entry point is not a file",
                entry_point_path.to_string_lossy()
            ));
        }

        validate_name(&manifest.name)?;

        // TODO add further validation if necessary:
        // - Check that the entry point is a supported format (.wasm?)
        // - Check that the entry point is appropriately signed
        Ok(Extension { path, manifest })
    }
}

/// Check extension name for validity.
pub fn validate_name(name: &str) -> Result<(), anyhow::Error> {
    if EXTENSION_NAME_RE.is_match(&name) {
        Ok(())
    } else {
        Err(anyhow!(
            "{}: invalid extension name, must be lowercase alphanumeric, dash (-) or underscore \
             (_)",
            name
        ))
    }
}

// Construct and return the extension path: $XDG_DATA_HOME/phylum/extensions
pub fn extensions_path() -> Result<PathBuf, anyhow::Error> {
    Ok(dirs::data_dir()?.join("phylum").join("extensions"))
}

fn extension_path(name: &str) -> Result<PathBuf, anyhow::Error> {
    Ok(extensions_path()?.join(name))
}