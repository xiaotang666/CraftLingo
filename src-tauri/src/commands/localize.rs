use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::core::models::{ImportTranslationsResult, InvalidTranslation, UntranslatedEntry};
use crate::core::{jobs, paths, shield};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntranslatedExportEntry {
    pub entry_id: String,
    pub mod_id: String,
    pub source_file: String,
    pub key: String,
    pub source_text: String,
    #[serde(default)]
    pub target_text: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UntranslatedExport {
    pub schema_version: u32,
    pub job_id: String,
    pub target_language: String,
    pub entries: Vec<UntranslatedExportEntry>,
}

fn read_untranslated(
    root: &std::path::Path,
    job_id: &str,
) -> Result<Vec<UntranslatedEntry>, String> {
    let path = paths::untranslated_path(root, job_id);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = std::fs::read_to_string(&path)
        .map_err(|e| format!("读取未翻译列表失败 ({}): {e}", path.display()))?;
    let mut entries = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let entry = serde_json::from_str::<UntranslatedEntry>(line)
            .map_err(|e| format!("解析未翻译条目失败: {e}"))?;
        entries.push(entry);
    }
    Ok(entries)
}

fn write_untranslated(
    root: &std::path::Path,
    job_id: &str,
    entries: &[UntranslatedEntry],
) -> Result<(), String> {
    let path = paths::untranslated_path(root, job_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("创建未翻译目录失败 ({}): {e}", parent.display()))?;
    }
    let mut buf = String::with_capacity(entries.len() * 160);
    for entry in entries {
        buf.push_str(
            &serde_json::to_string(entry).map_err(|e| format!("序列化未翻译条目失败: {e}"))?,
        );
        buf.push('\n');
    }
    std::fs::write(&path, buf).map_err(|e| format!("写入未翻译列表失败 ({}): {e}", path.display()))
}

/// Export all untranslated entries of a localize job into a single JSON file.
#[tauri::command]
pub fn export_untranslated(job_id: String, file_path: String) -> Result<usize, String> {
    info!(
        "export_untranslated: job_id={}, file_path={}",
        job_id, file_path
    );
    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let manager = jobs::JobManager::new(root.clone());
    let job = manager
        .load(&job_id)?
        .ok_or_else(|| format!("任务 {job_id} 未找到"))?;

    let entries = read_untranslated(&root, &job_id)?;
    let export = UntranslatedExport {
        schema_version: 1,
        job_id: job_id.clone(),
        target_language: job.target_language.clone(),
        entries: entries
            .iter()
            .map(|entry| UntranslatedExportEntry {
                entry_id: entry.entry_id.clone(),
                mod_id: entry.mod_id.clone(),
                source_file: entry.source_file.clone(),
                key: entry.key.clone(),
                source_text: entry.source_text.clone(),
                target_text: String::new(),
            })
            .collect(),
    };

    let content =
        serde_json::to_string_pretty(&export).map_err(|e| format!("序列化导出内容失败: {e}"))?;
    std::fs::write(&file_path, content)
        .map_err(|e| format!("写入导出文件失败 ({}): {e}", file_path))?;
    Ok(export.entries.len())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ImportItem {
    entry_id: String,
    #[serde(default)]
    target_text: String,
}

/// Import externally translated entries back into a localize job.
///
/// Only the job result file is written. The dictionary database is never
/// touched: AI-produced translations must not pollute the exact dictionary.
#[tauri::command]
pub fn import_untranslated_translations(
    job_id: String,
    file_path: String,
) -> Result<ImportTranslationsResult, String> {
    info!(
        "import_untranslated_translations: job_id={}, file_path={}",
        job_id, file_path
    );
    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let content = std::fs::read_to_string(&file_path)
        .map_err(|e| format!("读取导入文件失败 ({}): {e}", file_path))?;
    let value: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("解析导入 JSON 失败: {e}"))?;

    let array = match value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(mut map) => match map.remove("entries") {
            Some(serde_json::Value::Array(items)) => items,
            _ => return Err("导入 JSON 缺少 entries 数组".to_string()),
        },
        _ => return Err("导入 JSON 必须是数组或包含 entries 的对象".to_string()),
    };

    let mut items: Vec<ImportItem> = Vec::with_capacity(array.len());
    for raw in array {
        let item: ImportItem =
            serde_json::from_value(raw).map_err(|e| format!("导入条目格式错误: {e}"))?;
        items.push(item);
    }

    let untranslated = read_untranslated(&root, &job_id)?;
    let (accepted, result, imported_ids) = evaluate_import(&untranslated, &items);

    if !accepted.is_empty() {
        jobs::batch_append_results(&root, &job_id, &accepted)?;
        let remaining: Vec<UntranslatedEntry> = untranslated
            .into_iter()
            .filter(|entry| !imported_ids.contains(&entry.entry_id))
            .collect();
        write_untranslated(&root, &job_id, &remaining)?;
    }

    let manager = jobs::JobManager::new(root);
    manager.refresh_counts_from_results(&job_id)?;

    Ok(result)
}

/// Validate import items against the pending untranslated entries.
///
/// Pure (no IO) so the round-trip rules are unit-testable:
/// unknown ids, empty targets, and placeholder mismatches are rejected without
/// touching the dictionary.
fn evaluate_import(
    untranslated: &[UntranslatedEntry],
    items: &[ImportItem],
) -> (
    Vec<jobs::TranslationResult>,
    ImportTranslationsResult,
    HashSet<String>,
) {
    let index: HashMap<&str, &UntranslatedEntry> = untranslated
        .iter()
        .map(|entry| (entry.entry_id.as_str(), entry))
        .collect();

    let mut result = ImportTranslationsResult::default();
    let mut accepted: Vec<jobs::TranslationResult> = Vec::new();
    let mut imported_ids: HashSet<String> = HashSet::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for item in items {
        if item.entry_id.trim().is_empty() {
            result.invalid.push(InvalidTranslation {
                entry_id: item.entry_id.clone(),
                reason: "missingId".to_string(),
            });
            continue;
        }
        let Some(source) = index.get(item.entry_id.as_str()) else {
            result.invalid.push(InvalidTranslation {
                entry_id: item.entry_id.clone(),
                reason: "unknownEntry".to_string(),
            });
            continue;
        };
        if !seen_ids.insert(item.entry_id.clone()) {
            result.skipped += 1;
            continue;
        }
        let target_text = item.target_text.trim();
        if target_text.is_empty() {
            result.invalid.push(InvalidTranslation {
                entry_id: item.entry_id.clone(),
                reason: "emptyTarget".to_string(),
            });
            continue;
        }
        let shielded = shield::protect(&source.source_text);
        if !shield::validate(&shielded.tokens, target_text) {
            result.invalid.push(InvalidTranslation {
                entry_id: item.entry_id.clone(),
                reason: "placeholderMismatch".to_string(),
            });
            continue;
        }

        accepted.push(jobs::TranslationResult {
            key: source.key.clone(),
            source_text: source.source_text.clone(),
            target_text: target_text.to_string(),
            mod_id: source.mod_id.clone(),
            mod_name: source.mod_name.clone(),
            source_type: "imported".to_string(),
            source_file: source.source_file.clone(),
        });
        imported_ids.insert(item.entry_id.clone());
        result.imported += 1;
    }

    (accepted, result, imported_ids)
}

/// Drop all pending untranslated entries for a localize job.
#[tauri::command]
pub fn discard_untranslated(job_id: String) -> Result<usize, String> {
    info!("discard_untranslated: job_id={}", job_id);
    let root = paths::runtime_root().map_err(|e| e.to_string())?;
    let entries = read_untranslated(&root, &job_id)?;
    write_untranslated(&root, &job_id, &[])?;
    Ok(entries.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(id: &str, text: &str) -> UntranslatedEntry {
        UntranslatedEntry {
            entry_id: id.to_string(),
            mod_id: "examplemod".to_string(),
            mod_name: "examplemod.jar".to_string(),
            key: format!("item.{id}"),
            source_file: "assets/examplemod/lang/en_us.json".to_string(),
            format: "json".to_string(),
            language: "en_us".to_string(),
            source_text: text.to_string(),
        }
    }

    fn item(id: &str, target: &str) -> ImportItem {
        ImportItem {
            entry_id: id.to_string(),
            target_text: target.to_string(),
        }
    }

    #[test]
    fn accepts_valid_translation_and_keeps_position() {
        let pending = vec![pending("a1", "Hello")];
        let (accepted, result, ids) = evaluate_import(&pending, &[item("a1", "你好")]);
        assert_eq!(result.imported, 1);
        assert!(result.invalid.is_empty());
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].source_file, "assets/examplemod/lang/en_us.json");
        assert_eq!(accepted[0].source_type, "imported");
        assert!(ids.contains("a1"));
    }

    #[test]
    fn rejects_unknown_entry_id() {
        let pending = vec![pending("a1", "Hello")];
        let (accepted, result, _) = evaluate_import(&pending, &[item("nope", "你好")]);
        assert!(accepted.is_empty());
        assert_eq!(result.imported, 0);
        assert_eq!(result.invalid.len(), 1);
        assert_eq!(result.invalid[0].reason, "unknownEntry");
    }

    #[test]
    fn rejects_empty_target() {
        let pending = vec![pending("a1", "Hello")];
        let (accepted, result, _) = evaluate_import(&pending, &[item("a1", "   ")]);
        assert!(accepted.is_empty());
        assert_eq!(result.invalid[0].reason, "emptyTarget");
    }

    #[test]
    fn rejects_placeholder_mismatch() {
        let pending = vec![pending("a1", "Hello %s")];
        let (accepted, result, _) = evaluate_import(&pending, &[item("a1", "你好")]);
        assert!(accepted.is_empty());
        assert_eq!(result.invalid[0].reason, "placeholderMismatch");
    }

    #[test]
    fn accepts_preserved_placeholder() {
        let pending = vec![pending("a1", "Hello %s")];
        let (accepted, result, _) = evaluate_import(&pending, &[item("a1", "你好 %s")]);
        assert_eq!(result.imported, 1);
        assert_eq!(accepted.len(), 1);
    }

    #[test]
    fn duplicate_ids_are_skipped() {
        let pending = vec![pending("a1", "Hello")];
        let (accepted, result, _) =
            evaluate_import(&pending, &[item("a1", "你好"), item("a1", "您好")]);
        assert_eq!(result.imported, 1);
        assert_eq!(result.skipped, 1);
        assert_eq!(accepted.len(), 1);
    }
}
