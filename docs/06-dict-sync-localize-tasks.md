# 任务清单：VM 增强词典同步 + 自动汉化

对应规格：`docs/06-dict-sync-localize-spec.md`
分支：`feat/dict-sync-localize`

## T1 基础类型扩展
- [x] `src-tauri/src/core/jobs.rs`：`TranslationResult` 增 `#[serde(default)] pub source_file: String`；更新同文件 3 处测试构造。
- [x] `src-tauri/src/core/models.rs`：`Settings` 增 `i18n_dict_manifest_url`（`#[serde(default)]`，Default = `""`）；`PipelineConfig` 增 `localize_only: bool`；新增 `UntranslatedEntry`。
- [x] `src-tauri/src/core/paths.rs`：新增 `untranslated_path(root, job_id)`。
- 验证：`cargo check` 通过（此时 pipeline 构造处会有编译错误，随 T2 修复）。

## T2 pipeline 透传 source_file + 词典模式
- [x] `pipeline.rs`：`push_dict_entry` 增 `source_file` 参数并写入结果；`key_meta` 改 4 元组；`build_translation_result`/`map_translate_result` 透传；`retry_candidates_for_job` 与构造补字段；`dictionary.rs` 2 处测试构造补字段。
- [x] `pipeline.rs::LlmPhase::run`：`localize_only` 分支写 `_untranslated.jsonl`，不发 LLM，`llm_count=0`。
- [x] `commands/translate.rs`：`start_translation` 设 `localize_only=false`；新增 `start_auto_localize` 命令（事件通道同现有，`llm=None`）。
- 验证：`cargo test` 现有用例全绿。

## T3 打包按原路径
- [x] `packer.rs`：`PackEntry.source_file`；`output_relative_path`；按输出路径分组；`.lang`/`.json` 分别写出；`ConflictInfo` 增 `path`，冲突检测按 (path,key)。
- [x] `commands/pack.rs`：`generate_pack_from_job(..., allow_partial)`；跳过空 `target_text`/`untranslated`；映射 `source_file`。
- 验证：新增单测覆盖 3 场景（json 原路径、lang 原路径、同路径冲突）。

## T4 导出 / 导入未翻译
- [x] 新建 `src-tauri/src/commands/localize.rs`：`export_untranslated`、`import_untranslated_translations`（含 shield 占位符校验）、`discard_untranslated`。
- [x] `commands/mod.rs` + `lib.rs` 注册（含 `start_auto_localize`）。
- 验证：单测覆盖 entryId 未知 / 空译文 / 占位符不匹配 / 正常导入不写词典。

## T5 词典同步重构
- [x] `Cargo.toml` 增 `zstd`、`sha2`。
- [x] 新建 `src-tauri/src/core/i18n_sync.rs`：manifest 解析、`resolve_chain`、sha256、zstd 解压、增量 SQL 应用、参考库校验。
- [x] 重写 `commands/i18n_dict.rs`：manifest 优先 + CFPA 回退；增量（≤3）与全量保底；移除内置库回退与 LFS 测试；`active_i18n_dict_path` 无库时报错。
- [x] 删除 `src-tauri/resources/i18n-dict/` 内置引用与相关测试断言。
- 验证：`i18n_sync` 单测（chain/full 判定、sha256、zstd 往返、SQL 增量）；`cargo test`。

## T6 数据仓库流水线
- [x] 新建 `tools/dict-pipeline/build_dict.py`（合并 VM 覆盖自有、生成 SQLite/manifest/full/inc、保留 10 资产、内容不变不发布）。
- [x] 新建 `tools/dict-pipeline/.github/workflows/build-dict.yml`（`0 16 * * 0`）。
- [x] `tools/dict-pipeline/sources/own-dict.json` 占位（空数组）。
- 验证：本地用样例 JSON 运行脚本，产出 manifest 与资产可被客户端 `resolve_chain` 解析。

## T7 前端
- [x] `src/types.ts` 同步全部新增类型/字段。
- [x] `src/api/tauri.ts` 新增 4 个封装。
- [x] `DashboardPage.tsx`：自动汉化按钮 + 结果统计 + 导出/导入。
- [x] `SettingsPage.tsx`：词典卡片显示来源与版本。
- [x] `translations.ts`：新增文案 × 5 语言 + key 联合同步。
- 验证：`npm run test:unit`、`npm run build`、`npm run lint`（若有）。

## T8 文档与收尾
- [x] `CHANGELOG.md` 增条目。
- [x] `README.md` / `README.en-US.md` 说明数据仓库、自动汉化、导入不写词典。
- [x] `git status` 确认不含 `data/ logs/ dist/ build/ target/`。
- 验证：全量 `cargo test` + `npm run build` 通过。

## 依赖顺序
T1 → T2 → (T3, T4 并行) → T5 → T6 → T7 → T8
