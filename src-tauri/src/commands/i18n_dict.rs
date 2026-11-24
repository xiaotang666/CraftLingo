use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use reqwest::blocking::Client;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use tauri::Emitter;
use tracing::{info, warn};

use crate::core::{dictionary, i18n_sync, models::Settings, paths, settings};

/// 词典下载进度事件，前端监听 `i18n-dict-progress`。
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DictDownloadProgress {
    phase: &'static str,
    downloaded: u64,
    total: Option<u64>,
    percent: Option<u8>,
}

/// 增强词典（参考词条）单条记录，只读展示。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceDictEntry {
    pub source_text: String,
    pub target_text: String,
    pub mod_id: String,
    pub key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReferenceDictPage {
    pub total: usize,
    pub entries: Vec<ReferenceDictEntry>,
}

/// Default manifest URL for the dictionary repository.
/// Uses jsDelivr because raw.githubusercontent.com is unreliable in mainland China.
/// The URL can be overridden by `Settings.i18nDictManifestUrl` or the
/// `CRAFTLINGO_I18N_MANIFEST_URL` environment variable.
const DEFAULT_MANIFEST_URL: &str =
    "https://cdn.jsdelivr.net/gh/xiaotang666/CraftLingo-Dictionary@main/manifest.json";
const RAW_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/xiaotang666/CraftLingo-Dictionary/main/manifest.json";
/// 国内镜像前缀，按顺序轮询；词典只服务简中，因此下载时优先走镜像，最后回退官方直连。
const GITHUB_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://ghproxy.net/",
    "https://gh-proxy.net/",
];
const CFPA_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/CFPATools/i18n-dict/releases/latest";
const SQLITE_ASSET_NAME: &str = "Dict-Sqlite.db";

const META_TAG: &str = "i18n_dict_tag";
const META_PUBLISHED_AT: &str = "i18n_dict_published_at";
const META_ASSET_NAME: &str = "i18n_dict_asset_name";
const META_ENTRY_COUNT: &str = "i18n_dict_entry_count";
const META_SOURCE: &str = "i18n_dict_source";

const ACTIVE_DB_FILE_NAME: &str = "Dict-Sqlite.db";

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct I18nDictUpdateInfo {
    pub current_tag: Option<String>,
    pub latest_tag: String,
    pub latest_name: String,
    pub published_at: String,
    pub asset_name: String,
    pub reference_entries: usize,
    pub update_available: bool,
    /// `enhanced` (own data repo) or `official` (CFPATools fallback).
    pub source: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct I18nDictUpdateResult {
    pub tag: String,
    pub published_at: String,
    pub reference_entries: usize,
    pub source: String,
    pub used_incremental: bool,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    published_at: String,
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

struct ResolvedSource {
    manifest: i18n_sync::Manifest,
    /// `enhanced` or `official`.
    label: &'static str,
}

#[tauri::command]
pub async fn check_i18n_dict_update(app: tauri::AppHandle) -> Result<I18nDictUpdateInfo, String> {
    info!("check_i18n_dict_update");
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || check_i18n_dict_update_blocking(&app))
        .await
        .map_err(|e| format!("检查 i18n 模组词典更新线程崩溃: {e}"))?
}

fn check_i18n_dict_update_blocking(app: &tauri::AppHandle) -> Result<I18nDictUpdateInfo, String> {
    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let settings = settings::load_settings(&root).unwrap_or_default();
    let source = resolve_source(&settings)?;

    let conn = dictionary::open(&paths::dictionary_db_path(&root)).map_err(|e| e.to_string())?;
    let current_tag = dictionary::get_metadata(&conn, META_TAG).map_err(|e| e.to_string())?;
    let reference_entries = match active_i18n_dict_path(app)
        .and_then(|path| i18n_sync::validate_reference_db(&path).map_err(|e| e.to_string()))
    {
        Ok(count) => count,
        Err(error) => {
            warn!(%error, "本地 i18n 模组词典不可用");
            0
        }
    };
    let update_available =
        reference_entries == 0 || current_tag.as_deref() != Some(source.manifest.tag.as_str());

    Ok(I18nDictUpdateInfo {
        current_tag,
        latest_tag: source.manifest.tag.clone(),
        latest_name: if source.manifest.name.is_empty() {
            source.manifest.tag.clone()
        } else {
            source.manifest.name.clone()
        },
        published_at: source.manifest.published_at.clone(),
        asset_name: source
            .manifest
            .full
            .as_ref()
            .and_then(|asset| asset.url.rsplit('/').next().map(str::to_string))
            .unwrap_or_default(),
        reference_entries,
        update_available,
        source: source.label.to_string(),
    })
}

#[tauri::command]
pub async fn update_i18n_dict(app: tauri::AppHandle) -> Result<I18nDictUpdateResult, String> {
    info!("update_i18n_dict");
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || update_i18n_dict_blocking(&app))
        .await
        .map_err(|e| format!("更新 i18n 模组词典线程崩溃: {e}"))?
}

/// 分页查询增强词典（只读），供词典页浏览。
#[tauri::command]
pub async fn query_reference_dict(
    query: String,
    offset: u32,
    limit: u32,
) -> Result<ReferenceDictPage, String> {
    tauri::async_runtime::spawn_blocking(move || -> Result<ReferenceDictPage, String> {
        let root = paths::runtime_root().map_err(|e| e.to_string())?;
        let db_path = root.join("data").join("i18n-dict").join("Dict-Sqlite.db");
        if !db_path.is_file() {
            return Ok(ReferenceDictPage { total: 0, entries: Vec::new() });
        }
        let conn = Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("打开增强词典失败: {e}"))?;
        let escaped = query
            .trim()
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        let pattern = format!("%{escaped}%");
        let filter = "(ORIGIN_NAME LIKE ?1 ESCAPE '\\' OR TRANS_NAME LIKE ?1 ESCAPE '\\')";
        let total: i64 = conn
            .query_row(
                &format!("SELECT COUNT(*) FROM dict WHERE {filter}"),
                rusqlite::params![pattern],
                |row| row.get(0),
            )
            .map_err(|e| format!("统计增强词典失败: {e}"))?;
        let limit = limit.clamp(1, 500);
        let mut stmt = conn
            .prepare(&format!(
                "SELECT ORIGIN_NAME, TRANS_NAME, MODID, KEY FROM dict WHERE {filter} ORDER BY ID LIMIT ?2 OFFSET ?3"
            ))
            .map_err(|e| format!("准备增强词典查询失败: {e}"))?;
        let entries = stmt
            .query_map(
                rusqlite::params![pattern, limit, offset],
                |row| {
                    Ok(ReferenceDictEntry {
                        source_text: row.get(0)?,
                        target_text: row.get(1)?,
                        mod_id: row.get(2)?,
                        key: row.get(3)?,
                    })
                },
            )
            .map_err(|e| format!("查询增强词典失败: {e}"))?
            .filter_map(Result::ok)
            .collect();
        Ok(ReferenceDictPage {
            total: total.max(0) as usize,
            entries,
        })
    })
    .await
    .map_err(|e| format!("查询增强词典线程崩溃: {e}"))?
}

fn update_i18n_dict_blocking(app: &tauri::AppHandle) -> Result<I18nDictUpdateResult, String> {    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let settings = settings::load_settings(&root).unwrap_or_default();
    let source = resolve_source(&settings)?;
    let conn = dictionary::open(&paths::dictionary_db_path(&root)).map_err(|e| e.to_string())?;

    let current_tag = dictionary::get_metadata(&conn, META_TAG).map_err(|e| e.to_string())?;
    let plan = i18n_sync::resolve_chain(&source.manifest, current_tag.as_deref());
    info!(
        source = source.label,
        latest = %source.manifest.tag,
        current = ?current_tag,
        "i18n dict update plan: {:?}",
        plan
    );

    let (reference_entries, used_incremental) = match plan {
        i18n_sync::ChainPlan::Incremental(increments) => {
            match apply_increments(app, &root, &increments) {
                Ok(count) => (count, true),
                Err(error) => {
                    warn!(%error, "增量应用失败，回退全量下载");
                    (download_full(app, &root, &source.manifest)?, false)
                }
            }
        }
        i18n_sync::ChainPlan::Full { reason } => {
            info!(%reason, "i18n dict: full download");
            (download_full(app, &root, &source.manifest)?, false)
        }
    };

    let active_name = source
        .manifest
        .full
        .as_ref()
        .and_then(|asset| asset.url.rsplit('/').next().map(str::to_string))
        .unwrap_or_else(|| format!("full-{}.db.zst", source.manifest.tag));

    dictionary::set_metadata(&conn, META_TAG, &source.manifest.tag).map_err(|e| e.to_string())?;
    dictionary::set_metadata(&conn, META_PUBLISHED_AT, &source.manifest.published_at)
        .map_err(|e| e.to_string())?;
    dictionary::set_metadata(&conn, META_ASSET_NAME, &active_name).map_err(|e| e.to_string())?;
    dictionary::set_metadata(&conn, META_ENTRY_COUNT, &reference_entries.to_string())
        .map_err(|e| e.to_string())?;
    dictionary::set_metadata(&conn, META_SOURCE, source.label).map_err(|e| e.to_string())?;

    Ok(I18nDictUpdateResult {
        tag: source.manifest.tag,
        published_at: source.manifest.published_at,
        reference_entries,
        source: source.label.to_string(),
        used_incremental,
    })
}

/// Resolve the manifest to use: own data repository first, CFPA release second.
fn resolve_source(settings: &Settings) -> Result<ResolvedSource, String> {
    let client = manifest_client()?;

    for url in manifest_candidates(settings) {
        match fetch_manifest(&client, &url) {
            Ok(manifest) if manifest.full.is_some() => {
                info!(url = %url, "词典 manifest 已获取");
                return Ok(ResolvedSource {
                    manifest,
                    label: "enhanced",
                });
            }
            Ok(_) => warn!(url = %url, "manifest 缺少 full 资产，尝试下一个源"),
            Err(error) => warn!(%error, url = %url, "增强词典 manifest 拉取失败，尝试下一个源"),
        }
    }

    warn!("所有增强词典源均不可用，回退官方源");
    let official = fetch_official_manifest(&client)?;
    Ok(ResolvedSource {
        manifest: official,
        label: "official",
    })
}

/// GitHub 资源优先走国内镜像，最后回退官方直连。
fn with_mirrors(url: &str) -> Vec<String> {
    let is_github =
        url.contains("github.com/") || url.contains("raw.githubusercontent.com/");
    if !is_github {
        return vec![url.to_string()];
    }
    let mut urls: Vec<String> = GITHUB_MIRRORS
        .iter()
        .map(|mirror| format!("{mirror}{url}"))
        .collect();
    urls.push(url.to_string());
    urls
}

/// manifest 候选顺序：自定义地址 → jsDelivr → 镜像 raw → 官方 raw。
fn manifest_candidates(settings: &Settings) -> Vec<String> {
    if let Ok(from_env) = std::env::var("CRAFTLINGO_I18N_MANIFEST_URL") {
        if from_env.starts_with("http") {
            return with_mirrors(&from_env);
        }
    }
    let custom = settings.i18n_dict_manifest_url.trim();
    if custom.starts_with("http") {
        return with_mirrors(custom);
    }
    let mut urls = vec![DEFAULT_MANIFEST_URL.to_string()];
    urls.extend(with_mirrors(RAW_MANIFEST_URL));
    urls
}

fn fetch_manifest(client: &Client, url: &str) -> Result<i18n_sync::Manifest, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("请求 manifest 失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("manifest 响应异常: {e}"))?;
    let text = response
        .text()
        .map_err(|e| format!("读取 manifest 内容失败: {e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("解析 manifest 失败: {e}"))
}

/// Build a synthetic manifest from the official CFPA GitHub release so the
/// existing behaviour keeps working when the enhanced repo is unavailable.
fn fetch_official_manifest(client: &Client) -> Result<i18n_sync::Manifest, String> {
    let release: GithubRelease = client
        .get(CFPA_LATEST_RELEASE_URL)
        .send()
        .map_err(|e| format!("检查官方 i18n 词典失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("GitHub release 响应异常: {e}"))?
        .json()
        .map_err(|e| format!("解析官方 release 失败: {e}"))?;

    let asset = release
        .assets
        .into_iter()
        .find(|asset| asset.name == SQLITE_ASSET_NAME)
        .ok_or_else(|| format!("最新 release 中未找到 {SQLITE_ASSET_NAME}"))?;

    Ok(i18n_sync::Manifest {
        schema_version: 1,
        tag: release.tag_name.clone(),
        name: release.name.unwrap_or_else(|| release.tag_name.clone()),
        published_at: release.published_at,
        full: Some(i18n_sync::ManifestAsset {
            url: asset.browser_download_url,
            sha256: String::new(),
            size: 0,
            compression: "none".to_string(),
        }),
        increments: Vec::new(),
        ..Default::default()
    })
}

fn download_full(
    app: &tauri::AppHandle,
    root: &Path,
    manifest: &i18n_sync::Manifest,
) -> Result<usize, String> {
    let asset = manifest
        .full
        .as_ref()
        .ok_or_else(|| "manifest 未提供全量词典资产".to_string())?;
    let dir = i18n_dict_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 i18n 词典缓存目录失败: {e}"))?;

    let client = download_client()?;
    let is_zstd = asset.compression.eq_ignore_ascii_case("zstd");
    let downloaded = dir.join("Dict-Sqlite.download");
    download_to(app, &client, &asset.url, &downloaded, "full")?;
    verify_sha256(&downloaded, &asset.sha256)?;

    let next = dir.join(format!("{ACTIVE_DB_FILE_NAME}.next"));
    if is_zstd {
        i18n_sync::zstd_decompress_file(&downloaded, &next)
            .map_err(|e| format!("解压全量词典失败: {e}"))?;
        let _ = std::fs::remove_file(&downloaded);
    } else {
        if next.exists() {
            let _ = std::fs::remove_file(&next);
        }
        std::fs::rename(&downloaded, &next).map_err(|e| format!("保存下载词典失败: {e}"))?;
    }

    let count = i18n_sync::validate_reference_db(&next)?;
    replace_active_db(root, &next)?;
    Ok(count)
}

fn apply_increments(
    app: &tauri::AppHandle,
    root: &Path,
    increments: &[i18n_sync::ManifestIncrement],
) -> Result<usize, String> {
    let active = active_db_path(root);
    if !active.is_file() {
        return Err("本地词典不存在，无法增量".to_string());
    }
    let dir = i18n_dict_dir(root);
    let next = dir.join(format!("{ACTIVE_DB_FILE_NAME}.next"));
    std::fs::copy(&active, &next).map_err(|e| format!("复制本地词典失败: {e}"))?;

    let conn = Connection::open(&next).map_err(|e| format!("打开增量目标词典失败: {e}"))?;
    let client = download_client()?;

    for increment in increments {
        let packed = dir.join("increment.download");
        let sql_path = dir.join("increment.sql");
        download_to(app, &client, &increment.url, &packed, "increment")?;
        verify_sha256(&packed, &increment.sha256)?;
        if increment.compression.eq_ignore_ascii_case("zstd") {
            i18n_sync::zstd_decompress_file(&packed, &sql_path)
                .map_err(|e| format!("解压增量脚本失败: {e}"))?;
        } else {
            std::fs::rename(&packed, &sql_path).map_err(|e| format!("准备增量脚本失败: {e}"))?;
        }
        let sql =
            std::fs::read_to_string(&sql_path).map_err(|e| format!("读取增量脚本失败: {e}"))?;
        i18n_sync::apply_increment_sql(&conn, &sql)
            .map_err(|e| format!("应用增量脚本 {} 失败: {e}", increment.from_tag))?;
        let _ = std::fs::remove_file(&packed);
        let _ = std::fs::remove_file(&sql_path);
    }

    drop(conn);
    let count = i18n_sync::validate_reference_db(&next)?;
    replace_active_db(root, &next)?;
    Ok(count)
}

fn replace_active_db(root: &Path, next: &Path) -> Result<(), String> {
    let active = active_db_path(root);
    if let Some(parent) = active.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建 i18n 词典缓存目录失败: {e}"))?;
    }
    if active.exists() {
        std::fs::remove_file(&active).map_err(|e| format!("替换旧 i18n 模组词典失败: {e}"))?;
    }
    std::fs::rename(next, &active).map_err(|e| format!("保存 i18n 模组词典失败: {e}"))
}

fn verify_sha256(path: &Path, expected: &str) -> Result<(), String> {
    let expected = expected.trim();
    if expected.is_empty() {
        return Ok(());
    }
    let actual = i18n_sync::sha256_file(path).map_err(|e| format!("计算文件校验和失败: {e}"))?;
    if !actual.eq_ignore_ascii_case(expected) {
        return Err(format!("词典文件校验失败: 期望 {expected}, 实际 {actual}"));
    }
    Ok(())
}

/// 依次尝试镜像与官方直连，任一成功即返回。
fn download_to(
    app: &tauri::AppHandle,
    client: &Client,
    url: &str,
    dest: &Path,
    phase: &'static str,
) -> Result<(), String> {
    let mut last_error = String::new();
    for candidate in with_mirrors(url) {
        match download_once(app, client, &candidate, dest, phase) {
            Ok(()) => {
                if candidate != url {
                    info!(url = %candidate, "词典资源已通过镜像下载");
                }
                return Ok(());
            }
            Err(error) => {
                warn!(%error, url = %candidate, "词典资源下载失败，尝试下一个源");
                last_error = error;
            }
        }
    }
    Err(last_error)
}

fn emit_dict_progress(
    app: &tauri::AppHandle,
    phase: &'static str,
    downloaded: u64,
    total: Option<u64>,
) {
    let percent = total
        .filter(|t| *t > 0)
        .map(|t| ((downloaded.min(t) * 100 / t) as u8).min(100));
    let _ = app.emit(
        "i18n-dict-progress",
        DictDownloadProgress {
            phase,
            downloaded,
            total,
            percent,
        },
    );
}

fn download_once(
    app: &tauri::AppHandle,
    client: &Client,
    url: &str,
    dest: &Path,
    phase: &'static str,
) -> Result<(), String> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("创建下载目录失败: {e}"))?;
    }
    let mut file = File::create(dest).map_err(|e| format!("创建下载文件失败: {e}"))?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|e| format!("下载词典失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("词典下载响应异常: {e}"))?;
    let total = response.content_length();
    let mut downloaded = 0u64;
    let mut last_emit = 0u64;
    emit_dict_progress(app, phase, downloaded, total);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|e| format!("读取词典下载流失败: {e}"))?;
        if read == 0 {
            break;
        }
        std::io::Write::write_all(&mut file, &buffer[..read])
            .map_err(|e| format!("写入词典下载文件失败: {e}"))?;
        downloaded += read as u64;
        if downloaded - last_emit >= 256 * 1024 {
            last_emit = downloaded;
            emit_dict_progress(app, phase, downloaded, total);
        }
    }
    emit_dict_progress(app, phase, downloaded, total);
    Ok(())
}

pub(crate) fn active_i18n_dict_path(_app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let active = active_db_path(&root);
    if active.is_file() {
        return Ok(active);
    }
    Err("参考词典尚未同步，请在设置页检查并更新 i18n 词典".to_string())
}

fn i18n_dict_dir(root: &Path) -> PathBuf {
    root.join("data").join("i18n-dict")
}

fn active_db_path(root: &Path) -> PathBuf {
    i18n_dict_dir(root).join(ACTIVE_DB_FILE_NAME)
}

fn manifest_client() -> Result<Client, String> {
    Client::builder()
        .user_agent("CraftLingo")
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

fn download_client() -> Result<Client, String> {
    Client::builder()
        .user_agent("CraftLingo")
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_manifest_has_full_asset() {
        let manifest = i18n_sync::Manifest {
            schema_version: 1,
            tag: "v1".into(),
            full: Some(i18n_sync::ManifestAsset {
                url: "https://example.com/Dict-Sqlite.db".into(),
                compression: "none".into(),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert!(manifest.full.is_some());
        assert!(matches!(
            i18n_sync::resolve_chain(&manifest, None),
            i18n_sync::ChainPlan::Full { .. }
        ));
    }

    #[test]
    fn active_path_is_missing_before_first_sync() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!active_db_path(dir.path()).is_file());
    }

    #[test]
    fn github_urls_try_mirrors_before_direct() {
        let url = "https://github.com/o/r/releases/download/v1/a.zst";
        let urls = with_mirrors(url);
        assert_eq!(urls.len(), GITHUB_MIRRORS.len() + 1);
        assert!(urls[0].starts_with("https://ghfast.top/https://github.com/"));
        assert_eq!(urls.last().unwrap(), url);
    }

    #[test]
    fn non_github_urls_have_no_mirrors() {
        let url = "https://cdn.jsdelivr.net/gh/o/r@main/manifest.json";
        assert_eq!(with_mirrors(url), vec![url.to_string()]);
    }

    #[test]
    fn validate_rejects_non_database() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not.db");
        std::fs::write(&path, "version https://git-lfs.github.com/spec/v1\n").unwrap();
        assert!(i18n_sync::validate_reference_db(&path).is_err());
    }

    #[test]
    fn open_flags_constant_is_used() {
        // Guard against accidental removal of the read-only flag import.
        let _ = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY;
    }
}
