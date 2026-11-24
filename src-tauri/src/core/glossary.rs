use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// 汉化库仓库中的术语表目录。
pub const DEFAULT_REPO: &str = "xiaotang666/J20-Mcmodlanguage-Package";
pub const DEFAULT_BRANCH: &str = "main";
pub const DEFAULT_DIR: &str = "glossary";

/// GitHub API 镜像前缀（空串表示官方直连，优先尝试）。
const API_MIRRORS: &[&str] = &[
    "",
    "https://gh-proxy.com/",
    "https://gh-proxy.net/",
    "https://ghproxy.net/",
];

/// 原始文件镜像前缀。
const RAW_MIRRORS: &[&str] = &[
    "https://ghfast.top/",
    "https://gh-proxy.com/",
    "https://gh-proxy.net/",
    "https://ghproxy.net/",
    "",
];

const META_FILE: &str = "meta.json";
const MAX_PROMPT_TERMS: usize = 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryMeta {
    pub repo: String,
    pub branch: String,
    pub updated_at: String,
    pub files: Vec<GlossaryFile>,
}

impl Default for GlossaryMeta {
    fn default() -> Self {
        Self {
            repo: DEFAULT_REPO.to_string(),
            branch: DEFAULT_BRANCH.to_string(),
            updated_at: String::new(),
            files: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GlossaryFile {
    pub name: String,
    pub sha: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub struct RemoteFile {
    pub name: String,
    pub sha: String,
    pub size: u64,
    pub download_url: String,
}

pub fn glossary_dir(root: &Path) -> PathBuf {
    root.join("data").join("glossary")
}

fn meta_path(root: &Path) -> PathBuf {
    glossary_dir(root).join(META_FILE)
}

pub fn read_meta(root: &Path) -> GlossaryMeta {
    let path = meta_path(root);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => GlossaryMeta::default(),
    }
}

fn write_meta(root: &Path, meta: &GlossaryMeta) -> Result<(), String> {
    let dir = glossary_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("创建术语库目录失败: {e}"))?;
    let json = serde_json::to_string_pretty(meta).map_err(|e| format!("序列化术语库元数据失败: {e}"))?;
    fs::write(dir.join(META_FILE), json).map_err(|e| format!("写入术语库元数据失败: {e}"))
}

fn download_client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("CraftLingo/0.0.1")
        .build()
        .map_err(|e| format!("创建 HTTP 客户端失败: {e}"))
}

fn get_json(client: &Client, url: &str) -> Result<serde_json::Value, String> {
    let response = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|e| format!("请求术语库列表失败: {e}"))?
        .error_for_status()
        .map_err(|e| format!("术语库列表响应异常: {e}"))?;
    response
        .json::<serde_json::Value>()
        .map_err(|e| format!("解析术语库列表失败: {e}"))
}

/// 拉取汉化库 `glossary/` 目录的文件清单（带镜像回退）。
pub fn fetch_remote(client: &Client, repo: &str, branch: &str, dir: &str) -> Result<Vec<RemoteFile>, String> {
    let api = format!(
        "https://api.github.com/repos/{repo}/contents/{dir}?ref={branch}"
    );
    let mut last_error = String::new();
    for prefix in API_MIRRORS {
        let url = format!("{prefix}{api}");
        match get_json(client, &url) {
            Ok(value) => {
                let items = value
                    .as_array()
                    .ok_or_else(|| "术语库列表格式异常".to_string())?;
                let mut files = Vec::new();
                for item in items {
                    if item.get("type").and_then(|v| v.as_str()) != Some("file") {
                        continue;
                    }
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                    if !name.to_ascii_lowercase().ends_with(".json") {
                        continue;
                    }
                    files.push(RemoteFile {
                        name: name.to_string(),
                        sha: item.get("sha").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        size: item.get("size").and_then(|v| v.as_u64()).unwrap_or(0),
                        download_url: item
                            .get("download_url")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    });
                }
                if files.is_empty() {
                    return Err("术语库目录为空".to_string());
                }
                info!(count = files.len(), source = %url, "术语库文件列表获取成功");
                return Ok(files);
            }
            Err(error) => {
                warn!(%error, url = %url, "术语库列表获取失败，尝试下一个源");
                last_error = error;
            }
        }
    }
    Err(last_error)
}

fn download_file(client: &Client, remote: &RemoteFile, dest: &Path) -> Result<(), String> {
    let mut last_error = String::new();
    for prefix in RAW_MIRRORS {
        let url = format!("{prefix}{}", remote.download_url);
        match client.get(&url).send() {
            Ok(mut response) => {
                if !response.status().is_success() {
                    last_error = format!("{} 返回 {}", url, response.status());
                    continue;
                }
                let mut buffer = Vec::new();
                if let Err(error) = response.read_to_end(&mut buffer) {
                    last_error = format!("读取 {} 失败: {error}", url);
                    continue;
                }
                std::io::Write::write_all(
                    &mut fs::File::create(dest).map_err(|e| format!("写入术语库文件失败: {e}"))?,
                    &buffer,
                )
                .map_err(|e| format!("写入术语库文件失败: {e}"))?;
                return Ok(());
            }
            Err(error) => last_error = format!("{url}: {error}"),
        }
    }
    Err(last_error)
}

/// 下载术语库文件到本地，返回本地元数据。
pub fn sync_files(root: &Path, files: &[RemoteFile]) -> Result<GlossaryMeta, String> {
    let dir = glossary_dir(root);
    fs::create_dir_all(&dir).map_err(|e| format!("创建术语库目录失败: {e}"))?;
    let client = download_client()?;
    let mut local_files = Vec::new();
    for remote in files {
        let dest = dir.join(&remote.name);
        download_file(&client, remote, &dest)?;
        local_files.push(GlossaryFile {
            name: remote.name.clone(),
            sha: remote.sha.clone(),
            size: remote.size,
        });
    }
    let meta = GlossaryMeta {
        repo: DEFAULT_REPO.to_string(),
        branch: DEFAULT_BRANCH.to_string(),
        updated_at: chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        files: local_files,
    };
    write_meta(root, &meta)?;
    invalidate();
    Ok(meta)
}

/// 校验远端与本地是否一致，返回需要更新的文件。
pub fn changed_files(root: &Path, remote: &[RemoteFile]) -> (bool, Vec<RemoteFile>) {
    let meta = read_meta(root);
    let mut changed = Vec::new();
    for file in remote {
        let local = meta.files.iter().find(|f| f.name == file.name);
        let exists = glossary_dir(root).join(&file.name).is_file();
        match local {
            Some(local) if local.sha == file.sha && exists => {}
            _ => changed.push(file.clone()),
        }
    }
    // 本地有远端已删除的文件时也算需要同步
    let remote_names: HashSet<&str> = remote.iter().map(|f| f.name.as_str()).collect();
    let removed = meta
        .files
        .iter()
        .any(|f| !remote_names.contains(f.name.as_str()));
    (changed.is_empty() && !removed, changed)
}

fn parse_terms_from_value(value: &serde_json::Value, out: &mut Vec<(String, String)>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                parse_terms_from_value(item, out);
            }
        }
        serde_json::Value::Object(map) => {
            let pick = |keys: &[&str]| -> Option<&str> {
                keys.iter()
                    .find_map(|key| map.get(*key).and_then(|v| v.as_str()))
            };
            let source = pick(&["term", "origin_name", "originName", "source", "en"]);
            let target = pick(&["translation", "trans_name", "transName", "target", "zh"]);
            if let (Some(source), Some(target)) = (source, target) {
                if !source.trim().is_empty() && !target.trim().is_empty() {
                    out.push((source.trim().to_string(), target.trim().to_string()));
                }
                return;
            }
            // 纯映射形式：{"原文": "译文"}
            for (key, value) in map {
                if let Some(target) = value.as_str() {
                    if key != "term" && key != "translation" && !key.trim().is_empty() {
                        out.push((key.trim().to_string(), target.trim().to_string()));
                    }
                }
            }
        }
        _ => {}
    }
}

fn load_terms_from_dir(dir: &Path) -> Vec<(String, String)> {
    let mut terms = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return terms,
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| {
            path.is_file()
                && path.file_name().and_then(|n| n.to_str()) != Some(META_FILE)
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.eq_ignore_ascii_case("json"))
                    .unwrap_or(false)
        })
        .collect();
    paths.sort();
    for path in paths {
        let Ok(content) = fs::read_to_string(&path) else { continue };
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
            parse_terms_from_value(&value, &mut terms);
            continue;
        }
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                parse_terms_from_value(&value, &mut terms);
            }
        }
    }
    terms
}

static INDEX: OnceLock<Mutex<Option<Vec<(String, String)>>>> = OnceLock::new();

fn index_cell() -> &'static Mutex<Option<Vec<(String, String)>>> {
    INDEX.get_or_init(|| Mutex::new(None))
}

pub fn invalidate() {
    if let Ok(mut guard) = index_cell().lock() {
        *guard = None;
    }
}

/// 本地全部术语（懒加载缓存）。
pub fn all_terms() -> Vec<(String, String)> {
    let mut guard = match index_cell().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_none() {
        let root = crate::core::paths::runtime_root().unwrap_or_default();
        *guard = Some(load_terms_from_dir(&glossary_dir(&root)));
    }
    guard.clone().unwrap_or_default()
}

pub fn term_count() -> usize {
    all_terms().len()
}

/// 为一批待翻译文本挑选命中的术语，术语越长优先。
pub fn match_terms(texts: &[&str]) -> Vec<(String, String)> {
    let terms = all_terms();
    if terms.is_empty() {
        return Vec::new();
    }
    let lowered: Vec<String> = texts.iter().map(|t| t.to_lowercase()).collect();
    let mut matched: Vec<(String, String)> = terms
        .into_iter()
        .filter(|(source, _)| {
            let needle = source.to_lowercase();
            needle.chars().count() >= 2 && lowered.iter().any(|text| text.contains(&needle))
        })
        .collect();
    matched.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));
    matched.truncate(MAX_PROMPT_TERMS);
    matched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_term_object_and_mapping() {
        let value = serde_json::json!([
            { "term": "Diamond", "translation": "钻石" },
            { "origin_name": "Emerald", "trans_name": "绿宝石" },
            { "Stone": "石头" }
        ]);
        let mut terms = Vec::new();
        parse_terms_from_value(&value, &mut terms);
        assert_eq!(terms.len(), 3);
        assert!(terms.iter().any(|(s, t)| s == "Diamond" && t == "钻石"));
        assert!(terms.iter().any(|(s, t)| s == "Stone" && t == "石头"));
    }

    #[test]
    fn changed_files_detects_new_and_updated() {
        let root = std::env::temp_dir().join(format!("cl-glossary-{}", std::process::id()));
        let _ = fs::create_dir_all(glossary_dir(&root));
        let meta = GlossaryMeta {
            repo: DEFAULT_REPO.into(),
            branch: DEFAULT_BRANCH.into(),
            updated_at: "2026-01-01 00:00:00".into(),
            files: vec![GlossaryFile { name: "mods.json".into(), sha: "old".into(), size: 1 }],
        };
        write_meta(&root, &meta).unwrap();
        let remote = vec![
            RemoteFile { name: "mods.json".into(), sha: "new".into(), size: 1, download_url: String::new() },
            RemoteFile { name: "vanilla.json".into(), sha: "x".into(), size: 1, download_url: String::new() },
        ];
        let (up_to_date, changed) = changed_files(&root, &remote);
        assert!(!up_to_date);
        assert_eq!(changed.len(), 2);
        let _ = fs::remove_dir_all(&root);
    }
}
