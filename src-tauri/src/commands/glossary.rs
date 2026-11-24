use serde::Serialize;
use tracing::info;

use crate::core::{glossary, paths};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryStatus {
    pub installed: bool,
    pub files: usize,
    pub terms: usize,
    pub updated_at: String,
    pub repo: String,
    pub branch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryCheckResult {
    pub up_to_date: bool,
    pub changed_files: usize,
    pub remote_files: usize,
}

fn status_blocking() -> Result<GlossaryStatus, String> {
    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let meta = glossary::read_meta(&root);
    let installed = !meta.files.is_empty()
        && meta
            .files
            .iter()
            .all(|file| glossary::glossary_dir(&root).join(&file.name).is_file());
    Ok(GlossaryStatus {
        installed,
        files: meta.files.len(),
        terms: glossary::term_count(),
        updated_at: meta.updated_at,
        repo: glossary::DEFAULT_REPO.to_string(),
        branch: glossary::DEFAULT_BRANCH.to_string(),
    })
}

#[tauri::command]
pub async fn glossary_status() -> Result<GlossaryStatus, String> {
    tauri::async_runtime::spawn_blocking(status_blocking)
        .await
        .map_err(|e| format!("读取术语库状态线程崩溃: {e}"))?
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryTerm {
    pub source: String,
    pub target: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryTermPage {
    pub total: usize,
    pub entries: Vec<GlossaryTerm>,
}

#[tauri::command]
pub async fn list_glossary_terms(
    query: Option<String>,
    offset: usize,
    limit: usize,
) -> Result<GlossaryTermPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let q = query.unwrap_or_default().trim().to_lowercase();
        let all = glossary::all_terms();
        let filtered: Vec<(String, String)> = if q.is_empty() {
            all
        } else {
            all.into_iter()
                .filter(|(source, target)| {
                    source.to_lowercase().contains(&q) || target.to_lowercase().contains(&q)
                })
                .collect()
        };
        let total = filtered.len();
        let limit = limit.clamp(1, 500);
        let entries = filtered
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(source, target)| GlossaryTerm { source, target })
            .collect();
        Ok(GlossaryTermPage { total, entries })
    })
    .await
    .map_err(|e| format!("读取术语库线程崩溃: {e}"))?
}

#[tauri::command]
pub async fn check_glossary_update() -> Result<GlossaryCheckResult, String> {
    info!("check_glossary_update");
    tauri::async_runtime::spawn_blocking(|| -> Result<GlossaryCheckResult, String> {
        let root = paths::runtime_root().map_err(|e| e.to_string())?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("CraftLingo/0.0.1")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        let remote = glossary::fetch_remote(
            &client,
            glossary::DEFAULT_REPO,
            glossary::DEFAULT_BRANCH,
            glossary::DEFAULT_DIR,
        )?;
        let (up_to_date, changed) = glossary::changed_files(&root, &remote);
        Ok(GlossaryCheckResult {
            up_to_date,
            changed_files: changed.len(),
            remote_files: remote.len(),
        })
    })
    .await
    .map_err(|e| format!("检查术语库更新线程崩溃: {e}"))?
}

#[tauri::command]
pub async fn update_glossary() -> Result<GlossaryStatus, String> {
    info!("update_glossary");
    tauri::async_runtime::spawn_blocking(|| -> Result<GlossaryStatus, String> {
        let root = paths::runtime_root().map_err(|e| e.to_string())?;
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .user_agent("CraftLingo/0.0.1")
            .build()
            .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))?;
        let remote = glossary::fetch_remote(
            &client,
            glossary::DEFAULT_REPO,
            glossary::DEFAULT_BRANCH,
            glossary::DEFAULT_DIR,
        )?;
        glossary::sync_files(&root, &remote)?;
        status_blocking()
    })
    .await
    .map_err(|e| format!("更新术语库线程崩溃: {e}"))?
}
