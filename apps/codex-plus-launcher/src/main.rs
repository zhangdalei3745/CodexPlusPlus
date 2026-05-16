#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use anyhow::Result;
use codex_plus_core::launcher::{
    DefaultLaunchHooks, LaunchHooks, LaunchOptions, launch_and_inject_with_hooks,
};
use codex_plus_core::models::{DeleteResult, DeleteStatus, ExportResult, SessionRef};
use codex_plus_core::routes::{BridgeContext, BridgeDataService, CoreRuntimeService};
use codex_plus_core::user_scripts::UserScriptManager;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone)]
struct LauncherHooks {
    core: Arc<DefaultLaunchHooks>,
    data: Arc<LauncherDataService>,
}

impl Default for LauncherHooks {
    fn default() -> Self {
        Self {
            core: Arc::new(DefaultLaunchHooks::default()),
            data: Arc::new(LauncherDataService::default()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let hooks = LauncherHooks::default();
    let handle = launch_and_inject_with_hooks(LaunchOptions::default(), &hooks).await?;
    handle.wait_for_codex_exit().await?;
    Ok(())
}

#[async_trait::async_trait(?Send)]
impl LaunchHooks for LauncherHooks {
    fn resolve_app_dir(
        &self,
        app_dir: Option<&std::path::Path>,
    ) -> anyhow::Result<std::path::PathBuf> {
        self.core.resolve_app_dir(app_dir)
    }

    fn select_debug_port(&self, requested: u16) -> u16 {
        self.core.select_debug_port(requested)
    }

    fn select_helper_port(&self, requested: u16) -> u16 {
        self.core.select_helper_port(requested)
    }

    async fn load_settings(&self) -> anyhow::Result<codex_plus_core::settings::BackendSettings> {
        self.core.load_settings().await
    }

    async fn run_provider_sync(&self) -> anyhow::Result<()> {
        let _ = tokio::task::spawn_blocking(|| codex_plus_data::run_provider_sync(None))
            .await
            .map_err(|error| anyhow::anyhow!("provider sync task failed: {error}"))?;
        Ok(())
    }

    async fn start_helper(&self, helper_port: u16) -> anyhow::Result<()> {
        self.core.start_helper(helper_port).await
    }

    async fn launch_codex(
        &self,
        app_dir: &Path,
        debug_port: u16,
    ) -> anyhow::Result<codex_plus_core::launcher::CodexLaunch> {
        self.core.launch_codex(app_dir, debug_port).await
    }

    async fn bridge_context(&self, debug_port: u16) -> anyhow::Result<Option<BridgeContext>> {
        let runtime =
            CoreRuntimeService::new(debug_port, codex_plus_core::status::StatusStore::default())
                .with_user_scripts(default_user_script_manager());
        Ok(Some(BridgeContext::core_with_data(
            Arc::new(runtime),
            self.data.clone(),
        )))
    }

    async fn inject_bridge(
        &self,
        debug_port: u16,
        helper_port: u16,
        ctx: BridgeContext,
    ) -> anyhow::Result<()> {
        inject_with_context(debug_port, helper_port, ctx).await
    }

    async fn inject(&self, debug_port: u16, helper_port: u16) -> anyhow::Result<()> {
        self.core.inject(debug_port, helper_port).await
    }

    async fn write_status(&self, status: &str) {
        self.core.write_status(status).await;
    }

    async fn wait_for_codex_exit(
        &self,
        launch: &codex_plus_core::launcher::CodexLaunch,
    ) -> anyhow::Result<()> {
        self.core.wait_for_codex_exit(launch).await
    }

    async fn shutdown_helper(&self, helper_port: u16) {
        self.core.shutdown_helper(helper_port).await;
    }

    async fn terminate_codex(&self, launch: &codex_plus_core::launcher::CodexLaunch) {
        self.core.terminate_codex(launch).await;
    }
}

#[derive(Debug, Clone)]
struct LauncherDataService {
    db_path: Option<PathBuf>,
    backup_dir: PathBuf,
}

impl Default for LauncherDataService {
    fn default() -> Self {
        Self {
            db_path: default_codex_db_path(),
            backup_dir: codex_plus_core::paths::default_app_state_dir().join("backups"),
        }
    }
}

#[async_trait::async_trait]
impl BridgeDataService for LauncherDataService {
    async fn delete(&self, session: SessionRef) -> anyhow::Result<DeleteResult> {
        let Some(adapter) = self.storage_adapter() else {
            return Ok(DeleteResult {
                status: DeleteStatus::Failed,
                session_id: session.session_id,
                message: "No local database configured".to_string(),
                undo_token: None,
                backup_path: None,
            });
        };
        tokio::task::spawn_blocking(move || adapter.delete_local(&session))
            .await
            .map_err(|error| anyhow::anyhow!("delete task failed: {error}"))
    }

    async fn undo(&self, undo_token: String) -> anyhow::Result<DeleteResult> {
        let Some(adapter) = self.storage_adapter() else {
            return Ok(DeleteResult {
                status: DeleteStatus::Failed,
                session_id: String::new(),
                message: "No local backup adapter configured".to_string(),
                undo_token: Some(undo_token),
                backup_path: None,
            });
        };
        tokio::task::spawn_blocking(move || adapter.undo(&undo_token))
            .await
            .map_err(|error| anyhow::anyhow!("undo task failed: {error}"))
    }

    async fn export_markdown(&self, session: SessionRef) -> anyhow::Result<ExportResult> {
        let export_service = codex_plus_data::MarkdownExportService::new(self.db_path.clone());
        tokio::task::spawn_blocking(move || export_service.export(&session))
            .await
            .map_err(|error| anyhow::anyhow!("export markdown task failed: {error}"))
    }

    async fn find_archived_thread_by_title(
        &self,
        title: String,
    ) -> anyhow::Result<Option<SessionRef>> {
        let Some(adapter) = self.storage_adapter() else {
            return Ok(None);
        };
        tokio::task::spawn_blocking(move || adapter.find_archived_thread_by_title(&title))
            .await
            .map_err(|error| anyhow::anyhow!("archived lookup task failed: {error}"))
    }

    async fn move_thread_workspace(
        &self,
        session: SessionRef,
        target_cwd: String,
    ) -> anyhow::Result<Value> {
        let Some(adapter) = self.storage_adapter() else {
            return Ok(json!({
                "status": "failed",
                "session_id": session.session_id,
                "message": "No local database configured"
            }));
        };
        tokio::task::spawn_blocking(move || {
            adapter.move_codex_thread_workspace(&session, &target_cwd)
        })
        .await
        .map_err(|error| anyhow::anyhow!("move thread workspace task failed: {error}"))
    }

    async fn thread_sort_key(&self, session: SessionRef) -> anyhow::Result<Value> {
        let Some(adapter) = self.storage_adapter() else {
            return Ok(json!({
                "status": "failed",
                "session_id": session.session_id,
                "message": "No local database configured"
            }));
        };
        tokio::task::spawn_blocking(move || adapter.codex_thread_sort_key(&session))
            .await
            .map_err(|error| anyhow::anyhow!("thread sort key task failed: {error}"))
    }

    async fn thread_sort_keys(&self, sessions: Vec<SessionRef>) -> anyhow::Result<Value> {
        let Some(adapter) = self.storage_adapter() else {
            return Ok(json!({
                "status": "failed",
                "message": "No local database configured",
                "sort_keys": []
            }));
        };
        tokio::task::spawn_blocking(move || adapter.codex_thread_sort_keys(&sessions))
            .await
            .map_err(|error| anyhow::anyhow!("thread sort keys task failed: {error}"))
    }
}

impl LauncherDataService {
    fn storage_adapter(&self) -> Option<codex_plus_data::SQLiteStorageAdapter> {
        Some(codex_plus_data::SQLiteStorageAdapter::new(
            self.db_path.clone()?,
            codex_plus_data::BackupStore::new(self.backup_dir.clone()),
        ))
    }
}

async fn inject_with_context(
    debug_port: u16,
    helper_port: u16,
    ctx: BridgeContext,
) -> anyhow::Result<()> {
    let targets = codex_plus_core::cdp::list_targets(debug_port).await?;
    let target = codex_plus_core::cdp::pick_page_target(&targets)?;
    let websocket_url = target
        .web_socket_debugger_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("selected CDP target has no websocket URL"))?;
    let script = codex_plus_core::assets::injection_script(helper_port);
    let user_bundle = default_user_script_manager()
        .build_enabled_bundle()
        .unwrap_or_default();
    let new_document_scripts = if user_bundle.is_empty() {
        vec![script]
    } else {
        vec![script, user_bundle]
    };
    codex_plus_core::bridge::install_bridge(
        websocket_url,
        codex_plus_core::bridge::BRIDGE_BINDING_NAME,
        Arc::new(move |path, payload| {
            let ctx = ctx.clone();
            Box::pin(async move {
                Ok(codex_plus_core::routes::handle_bridge_request(ctx, &path, payload).await)
            })
        }),
        &new_document_scripts,
    )
    .await
}

fn default_codex_db_path() -> Option<PathBuf> {
    let path = directories::BaseDirs::new()?
        .home_dir()
        .join(".codex")
        .join("state_5.sqlite");
    path.exists().then_some(path)
}

fn default_user_script_manager() -> UserScriptManager {
    let config_dir = default_user_scripts_config_dir();
    UserScriptManager::new(
        builtin_user_scripts_dir(),
        config_dir.join("user_scripts"),
        config_dir.join("user_scripts.json"),
    )
}

fn default_user_scripts_config_dir() -> PathBuf {
    if cfg!(windows) {
        if let Some(roaming) = std::env::var_os("APPDATA") {
            return PathBuf::from(roaming).join("Codex++");
        }
        if let Some(home) = directories::BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
            return home.join("AppData").join("Roaming").join("Codex++");
        }
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| directories::BaseDirs::new().map(|dirs| dirs.home_dir().join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("Codex++")
}

fn builtin_user_scripts_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .map(|path| path.join("user_scripts"))
        .unwrap_or_else(|| PathBuf::from("user_scripts"))
}
