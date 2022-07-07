//! Deno runtime for extensions.

use std::pin::Pin;
use std::rc::Rc;
use std::sync::Arc;
use std::thread;

use anyhow::{anyhow, Result};
use deno_ast::{MediaType, ParseParams, SourceTextInfo};
use deno_runtime::deno_core::{
    self, Extension, ModuleLoader, ModuleSource, ModuleSourceFuture, ModuleSpecifier, ModuleType,
};
use deno_runtime::permissions::Permissions;
use deno_runtime::worker::{MainWorker, WorkerOptions};
use deno_runtime::{colors, BootstrapOptions};
use tokio::fs;
use url::{Host, Url};

use crate::commands::extensions::api;
use crate::commands::extensions::extension::{self, ExtensionState};

/// Load Phylum API for module injection.
const EXTENSION_API: &str = include_str!("./extension_api.ts");

/// Execute Phylum extension.
pub async fn run(
    extension_state: ExtensionState,
    extension: &extension::Extension,
    args: Vec<String>,
) -> Result<()> {
    let phylum_api = Extension::builder().ops(api::api_decls()).build();

    let main_module = deno_core::resolve_path(&extension.path().to_string_lossy())?;

    let cpu_count = thread::available_parallelism().map(|p| p.get()).unwrap_or(1);

    let bootstrap = BootstrapOptions {
        cpu_count,
        args,
        runtime_version: env!("CARGO_PKG_VERSION").into(),
        user_agent: "phylum-cli/extension".into(),
        no_color: !colors::use_color(),
        is_tty: colors::is_tty(),
        enable_testing_features: Default::default(),
        debug_flag: Default::default(),
        ts_version: Default::default(),
        location: Default::default(),
        unstable: Default::default(),
    };

    let options = WorkerOptions {
        bootstrap,
        web_worker_preload_module_cb: Arc::new(|_| unimplemented!("web workers are not supported")),
        create_web_worker_cb: Arc::new(|_| unimplemented!("web workers are not supported")),
        module_loader: Rc::new(ExtensionsModuleLoader),
        extensions: vec![phylum_api],
        seed: None,
        unsafely_ignore_certificate_errors: Default::default(),
        should_break_on_first_statement: Default::default(),
        compiled_wasm_module_store: Default::default(),
        shared_array_buffer_store: Default::default(),
        maybe_inspector_server: Default::default(),
        format_js_error_fn: Default::default(),
        get_error_class_fn: Default::default(),
        origin_storage_dir: Default::default(),
        broadcast_channel: Default::default(),
        source_map_getter: Default::default(),
        root_cert_store: Default::default(),
        blob_store: Default::default(),
        stdio: Default::default(),
    };

    // Build permissions object from extension's requested permissions.
    let permissions = Permissions::from_options(&extension.permissions().try_into()?);

    // Initialize Deno runtime.
    let mut worker = MainWorker::bootstrap_from_options(main_module.clone(), permissions, options);

    // Export shared state.
    worker.js_runtime.op_state().borrow_mut().put(extension_state);

    // Execute extension code.
    worker.execute_main_module(&main_module).await?;
    worker.run_event_loop(false).await
}

/// See https://github.com/denoland/deno/blob/main/core/examples/ts_module_loader.rs.
struct ExtensionsModuleLoader;

impl ExtensionsModuleLoader {
    async fn load_from_filesystem(path: &Url) -> Result<String> {
        let path = path.to_file_path().map_err(|_| anyhow!("{path:?}: is not a path"))?;

        let extensions_path = extension::extensions_path()?;
        if !path.starts_with(&extensions_path) {
            return Err(anyhow!(
                "`{}`: importing from paths outside of the extension's directory is not allowed",
                path.to_string_lossy(),
            ));
        }

        if path.is_symlink() {
            return Err(anyhow!(
                "`{}`: importing from symlinks is not allowed",
                path.to_string_lossy(),
            ));
        }

        Ok(fs::read_to_string(path).await?)
    }

    async fn load_from_deno_std(path: &Url) -> Result<String> {
        if let Some(Host::Domain("deno.land")) = path.host() {
            let response = reqwest::get(path.clone()).await?;
            Ok(response.text().await?)
        } else {
            Err(anyhow!(
                "`{}`: importing from domains other than `deno.land` is not allowed",
                path.host().unwrap_or(Host::Domain("<unknown host>"))
            ))
        }
    }
}

impl ModuleLoader for ExtensionsModuleLoader {
    fn resolve(&self, specifier: &str, referrer: &str, _is_main: bool) -> Result<ModuleSpecifier> {
        if specifier == "phylum" {
            Ok(ModuleSpecifier::parse("deno:phylum")?)
        } else {
            Ok(deno_core::resolve_import(specifier, referrer)?)
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<ModuleSpecifier>,
        _is_dyn_import: bool,
    ) -> Pin<Box<ModuleSourceFuture>> {
        let module_specifier = module_specifier.clone();
        Box::pin(async move {
            // Inject Phylum API module.
            if module_specifier.as_str() == "deno:phylum" {
                return phylum_module();
            }

            // Determine source file type.
            // We do not care about invalid URLs yet: This match statement is inexpensive,
            // bears no risk and does not do I/O -- it operates fully off of the
            // contents of the URL.
            let media_type = MediaType::from(&module_specifier);
            let (module_type, should_transpile) = match media_type {
                MediaType::JavaScript | MediaType::Mjs | MediaType::Cjs => {
                    (ModuleType::JavaScript, false)
                },
                MediaType::TypeScript
                | MediaType::Jsx
                | MediaType::Mts
                | MediaType::Cts
                | MediaType::Dts
                | MediaType::Dmts
                | MediaType::Dcts
                | MediaType::Tsx => (ModuleType::JavaScript, true),
                MediaType::Json => (ModuleType::Json, false),
                _ => return Err(anyhow!("Unknown JS module format: {}", module_specifier)),
            };

            // Load either a local file under the extensions directory, or a Deno standard
            // library module. Reject all URLs that do not fit these two use
            // cases.
            let mut code = match module_specifier.scheme() {
                "file" => ExtensionsModuleLoader::load_from_filesystem(&module_specifier).await?,
                "https" => ExtensionsModuleLoader::load_from_deno_std(&module_specifier).await?,
                _ => return Err(anyhow!("Unsupported module specifier: {}", module_specifier)),
            };

            if should_transpile {
                code = transpile(module_specifier.to_string(), code, media_type)?;
            }

            Ok(ModuleSource {
                code: code.into_bytes().into_boxed_slice(),
                module_url_specified: module_specifier.to_string(),
                module_url_found: module_specifier.to_string(),
                module_type,
            })
        })
    }
}

/// Transpile code to JavaScript.
fn transpile(
    specifier: impl Into<String>,
    code: impl Into<String>,
    media_type: MediaType,
) -> Result<String> {
    let parsed = deno_ast::parse_module(ParseParams {
        text_info: SourceTextInfo::from_string(code.into()),
        specifier: specifier.into(),
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
        media_type,
    })?;
    Ok(parsed.transpile(&Default::default())?.text)
}

/// Load the internal Phylum API module
fn phylum_module() -> Result<ModuleSource> {
    let module_url = "deno:phylum";
    let code = transpile(module_url, EXTENSION_API, MediaType::TypeScript)?;

    Ok(ModuleSource {
        code: code.into_bytes().into_boxed_slice(),
        module_url_specified: module_url.into(),
        module_url_found: module_url.into(),
        module_type: ModuleType::JavaScript,
    })
}