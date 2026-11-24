use crate::commands::localize;
use crate::core::{ai_requirements, packer, paths, settings};
use tracing::info;

#[tauri::command]
pub fn copy_pack_to_instance(
    pack_zip_path: String,
    instance_path: String,
    overwrite: bool,
) -> Result<packer::CopyResult, String> {
    info!(
        "copy_pack_to_instance: pack={}, instance={}, overwrite={}",
        pack_zip_path, instance_path, overwrite
    );
    packer::copy_to_resourcepacks(&pack_zip_path, &instance_path, overwrite)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn generate_pack_from_job(
    job_id: String,
    target_language: String,
    dry_run: bool,
    output_dir: Option<String>,
    allow_partial: Option<bool>,
    include_untranslated: Option<bool>,
) -> Result<packer::PackResult, String> {
    let allow_partial = allow_partial.unwrap_or(false);
    let include_untranslated = include_untranslated.unwrap_or(false);
    info!(
        "generate_pack_from_job: job_id={}, target_language={}, dry_run={}, output_dir={:?}, allow_partial={}, include_untranslated={}",
        job_id, target_language, dry_run, output_dir, allow_partial, include_untranslated
    );

    let result = tauri::async_runtime::spawn_blocking(move || {
        let root = paths::runtime_root().map_err(|e| e.to_string())?;
        let manager = crate::core::jobs::JobManager::new(root.clone());

        let job = manager.refresh_counts_from_results(&job_id)?;
        if job.failed_entries > 0 && !allow_partial {
            return Err(format!(
                "翻译任务 {job_id} 仍有 {} 条失败或缺失条目，无法生成资源包",
                job.failed_entries
            ));
        }
        let scan_summary = manager
            .load_scan_summary(&job.scan_job_id)
            .map_err(|e| format!("读取翻译任务关联扫描结果失败: {e}"))?;

        let results = manager.load_results(&job_id)?;

        let entries: Vec<packer::PackEntry> = results
            .iter()
            .filter(|r| !r.target_text.is_empty() && r.source_type != "untranslated")
            .map(|r| packer::PackEntry {
                mod_id: r.mod_id.clone(),
                key: r.key.clone(),
                text: r.target_text.clone(),
                source_text: r.source_text.clone(),
                source_file: r.source_file.clone(),
                source_type: r.source_type.clone(),
            })
            .collect();

        let output_dir = output_dir
            .map(|p| std::path::PathBuf::from(p))
            .unwrap_or_else(|| paths::build_output_dir(&root));
        std::fs::create_dir_all(&output_dir).map_err(|e| e.to_string())?;

        // 从 settings 读取 outputPackName，替换占位符
        let root_path = root.clone();
        let (build_name, pack_format) = {
            let s =
                settings::load_settings(&root_path).map_err(|e| format!("加载设置失败: {e}"))?;
            let instance_path = scan_summary.instance_path.as_str();
            match settings::detect_mc_version(instance_path) {
                Ok(ver) => (
                    settings::replace_version_placeholder(&s.output_pack_name, &ver),
                    packer::pack_format_for_mc_version(&ver),
                ),
                Err(e) => return Err(format!("MC 版本检测失败，无法确定资源包 pack_format: {e}")),
            }
        };

        // 关闭 AI 的全自动流程：先输出待翻译文件与要求说明，保证即使没有
        // 任何词典命中，用户也能拿到可交付的文件。
        if include_untranslated && !dry_run {
            let safe_build_name = packer::sanitize_name(&build_name);
            let untranslated_path = output_dir.join(format!(
                "{}-{}-待翻译.json",
                safe_build_name, target_language
            ));
            let count =
                localize::export_untranslated_to_path(&root, &job_id, &untranslated_path)?;
            info!(
                "generate_pack_from_job: 已输出 {} 条待翻译条目 → {}",
                count,
                untranslated_path.display()
            );

            let requirements_path = output_dir.join("ai翻译要求.txt");
            std::fs::write(
                &requirements_path,
                ai_requirements::render(&target_language),
            )
            .map_err(|e| format!("写入 ai翻译要求.txt 失败: {e}"))?;

            if entries.is_empty() {
                return Err(format!(
                    "没有可打包的词典译文，已输出待翻译文件（{} 条）与 ai翻译要求.txt",
                    count
                ));
            }
        }

        if entries.is_empty() {
            return Err("翻译结果为空或所有条目均未通过校验，无法生成资源包".to_string());
        }

        let options = packer::PackOptions {
            target_language,
            entries,
            build_name,
            dry_run,
            output_dir: output_dir.to_string_lossy().to_string(),
            pack_format,
            icon_path: None,
        };

        packer::generate_pack(&options).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("打包线程崩溃: {e}"))??;

    Ok(result)
}
