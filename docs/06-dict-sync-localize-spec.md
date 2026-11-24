# 规格：VM 增强词典同步 + 自动汉化（词典模式）

状态：待批准
基线：v0.2.9（commit `824d6a0`）
分支：`feat/dict-sync-localize`

## 1. 目标与已确认约束

### 1.1 词典同步
- 不再内置 `Dict-Sqlite.db`（现为 LFS 指针，不可用）。
- 首次启动后从「本项目数据仓库」拉取；服务端构建时间为**每周一 00:00（北京时间）**。
- 客户端行为保持不变：**启动时检查 + 设置页手动**。
- 数据仓库合并顺序：自有汉化库 → VM 词条，冲突时 **VM 覆盖自有**（服务端构建期完成，客户端只下载合并结果）。
- 自有汉化库尚未就绪：仅预留 `sources/own-dict.json` 占位（格式同 VM 的 `Dict.json`），缺失则跳过。
- 增量策略：本地落后 **≤ 3 个资产** → 按增量逐步应用；**> 3 个资产**、无基线、链条断裂或任一增量失败 → **全量下载保底**。
- 资产保留 10 个（1 全量 + 9 增量，每个增量对应一个基准 tag）。
- 下载产物统一 `zstd` 压缩 + `sha256` 校验。

### 1.2 自动汉化
- 新按钮只做**词典匹配**，**不调用 LLM**。
- 扫描 `assets/**/lang/`（jar 内与资源包均复用现有 scanner）。
- 词典命中 → 直接生成 `zh_cn` 语言文件；未命中 → 导出 JSON 供外部翻译后**原位导入**。
- 必须支持：一个 mod 多个 lang 文件夹（多 namespace）、多 mod 批量导入；导入严禁串位。
- 导入**只写 job 结果文件，绝不修改词典**（AI 译文须保持词典精确性）。
- 导出使用 JSON（非 JSONL）。

## 2. 现状关键事实（已核实）

| 位置 | 事实 |
|---|---|
| `src-tauri/src/commands/i18n_dict.rs` | 固定 CFPA GitHub Release，下载 `Dict-Sqlite.db` 到 `data/i18n-dict/`，metadata 键 `i18n_dict_tag` 等；有内置库回退（需移除） |
| `src-tauri/src/core/dictionary.rs:931` | `MemoryDictionary::load_with_reference_db` 以只读打开参考库；SQL 依赖表 `dict`、列 `ORIGIN_NAME/TRANS_NAME`、索引 `dict_index` |
| `src-tauri/src/core/scanner.rs:1155` | `parse_lang_path` 要求 `assets/<ns>/lang/<file>` 四段；`mod_id` 即 namespace |
| `src-tauri/src/core/scanner.rs:1118` | `LanguageEntry.source_file` = `assets/<ns>/lang/<locale>.<ext>` |
| `src-tauri/src/core/packer.rs:224` | 当前按 `mod_id` 输出 `assets/<modid>/lang/<target>.json`，丢失 `.lang` 扩展名与真实文件名，无同路径冲突检测 |
| `src-tauri/src/core/jobs.rs:59` | `TranslationResult` 无 `source_file` 字段 |
| `src-tauri/src/core/pipeline.rs:1017` | `LlmPhase` 必须有 `MemoryDictionary` 且必定调用 LLM |
| `src-tauri/src/commands/pack.rs:35` | `failed_entries > 0` 直接拒绝打包 |
| 前端 | `src/types.ts`、`src/api/tauri.ts`、`src/pages/SettingsPage.tsx`、`src/pages/DashboardPage.tsx`、`src/i18n/translations.ts`（5 语言） |

## 3. 设计

### 3.1 数据仓库产物格式（服务端）

`manifest.json`：
```json
{
  "schemaVersion": 1,
  "tag": "2026-11-23",
  "name": "CraftLingo 增强词典 2026-11-23",
  "publishedAt": "2026-11-23T00:00:00+08:00",
  "entryCount": 0,
  "contentHash": "sha256:...",
  "full": { "url": "https://.../full-2026-11-23.db.zst", "sha256": "...", "size": 0, "compression": "zstd" },
  "increments": [
    { "fromTag": "2026-11-16", "url": "https://.../inc-2026-11-16-2026-11-23.sql.zst", "sha256": "...", "size": 0, "compression": "zstd" }
  ]
}
```

- `full-<tag>.db.zst`：zstd 压缩的完整 SQLite 文件，schema 必须满足参考库 SQL：
  `CREATE TABLE dict(ID INTEGER PRIMARY KEY AUTOINCREMENT, ORIGIN_NAME TEXT NOT NULL, TRANS_NAME TEXT NOT NULL, MODID TEXT NOT NULL, KEY TEXT NOT NULL, VERSION TEXT NOT NULL, CURSEFORGE TEXT NOT NULL);`
  `CREATE INDEX dict_index ON dict(ORIGIN_NAME);`
- `inc-<from>-<to>.sql.zst`：zstd 压缩的 UTF-8 SQL 脚本，单事务：
  `BEGIN; INSERT OR REPLACE INTO dict(...) VALUES(...); DELETE FROM dict WHERE ID IN (...); COMMIT;`
- 保留最近 10 个资产；内容 `contentHash` 未变化则不发布新 Release。

### 3.2 客户端词典同步

新增 `src-tauri/src/core/i18n_sync.rs`（纯逻辑，可单测）：
- `Manifest` / `ManifestAsset` / `ManifestIncrement`（serde camelCase + default）。
- `pub fn resolve_chain(manifest, current_tag) -> ChainPlan`，枚举：
  - `Full { reason }`（current 缺失 / 链条断裂 / 步数 > 3）
  - `Incremental(Vec<ManifestIncrement>)`
- `pub fn sha256_file(path) -> io::Result<String>`
- `pub fn zstd_decompress_file(src, dst) -> io::Result<()>`
- `pub fn apply_increment_sql(conn, sql) -> rusqlite::Result<()>`（`execute_batch` 包裹事务）
- `pub fn validate_reference_db(path) -> Result<usize, String>`（存在 `dict` 表 + 非空条目计数）

改造 `src-tauri/src/commands/i18n_dict.rs`：
- 保留命令名 `check_i18n_dict_update` / `update_i18n_dict`（前端无需改调用点）。
- `fetch_manifest()`：读取 `Settings.i18nDictManifestUrl`（为 http(s) 时优先）→ 否则常量 `DEFAULT_MANIFEST_URL`（占位，指向用户数据仓库 raw 地址）→ 失败回退 CFPA Release（现有逻辑保留为 `official` 源）。
- `update_i18n_dict_blocking`：
  1. 取 `current_tag`，`resolve_chain`。
  2. `Incremental`：把当前库复制到 `data/i18n-dict/Dict-Sqlite.db.next`，逐个下载 → sha256 → zstd 解压 → `apply_increment_sql`，全部成功才原子替换。
  3. `Full` 或增量抛错：下载 full → sha256 → 解压 → `validate_reference_db` → 原子替换。
  4. 成功后写 metadata：`i18n_dict_tag`、`i18n_dict_published_at`、`i18n_dict_source`、`i18n_dict_entry_count`、`i18n_dict_applied_increments`。
- 移除 `bundled_i18n_dict` / `BUNDLED_*` 及 LFS 相关测试；`active_i18n_dict_path` 找不到文件时返回错误「词典尚未同步，请在设置页更新」。
- 依赖：`zstd`、`sha2`（Rust crate）。

`core/models.rs` + `src/types.ts` 新增 Settings 字段：
- `i18n_dict_manifest_url: String`（默认 `""` → 用内置常量）。

### 3.3 自动汉化（词典模式）

`core/models.rs`：
- `PipelineConfig` 增加 `localize_only: bool`。
- 新增 `UntranslatedEntry`（camelCase）：`entry_id, mod_id, mod_name, key, source_file, format, language, source_text`。
- `entry_id = hash_text(format!("{mod_id}|{source_file}|{key}|{text_hash}"))`，`hash_text` 复用 `dictionary::hash_text`（FNV-1a，稳定）。

`core/paths.rs`：
- `pub fn untranslated_path(root, job_id) -> PathBuf` → `data/jobs/{job_id}_untranslated.jsonl`。

`core/pipeline.rs`：
- `LlmPhase::run`：若 `ctx.config.localize_only`，把 `llm_only_entries` 写成 `UntranslatedEntry` JSONL 到 `untranslated_path`，`llm_count = 0`，发送 `entry_progress` 状态 `Failed`（errorMessage 固定 `untranslated`）后 `Continue`；不触碰词典。
- `TranslationResult` 增加 `#[serde(default)] pub source_file: String`：
  - `push_dict_entry` 增加 `source_file` 参数（调用处传 `entry.source_file`）。
  - LLM 元组 `key_meta` 由 3 元扩为 4 元 `(key, mod_id, file_name, source_file)`；`build_translation_result` / `map_translate_result` 透传。
  - `retry_candidates_for_job` 的 `meta` 与构造补 `source_file`（空串可接受）。
  - `jobs.rs` 测试、`dictionary.rs` 测试构造处补 `source_file`。

`commands/translate.rs`：
- `start_translation` 复用现有流程，`PipelineConfig.localize_only = false`。
- 新增命令 `start_auto_localize(app, path, source_language, target_language, scan_job_id) -> LocalizeStartResult { job_id, total, dictionary_hits, untranslated }`：
  - 与 `start_translation` 相同的事件通道与 job 创建；
  - `llm: None`，`localize_only: true`；
  - 返回 job_id 与统计（`PipelineResult` 扩展或在命令内重载）。

### 3.4 打包按原路径
`core/packer.rs`：
- `PackEntry` 增加 `#[serde(default)] pub source_file: String`。
- `fn output_relative_path(entry, target_language) -> String`：
  - 若 `source_file` 匹配 `assets/<ns>/lang/<file>`：把 `<file>` 的 locale 段替换为 `target_language`，扩展名保持不变（`.json` 保持 json，`.lang` 保持 lang）。
  - 否则回退 `assets/{mod_id}/lang/{target_language}.json`。
- 分组键改为 `output_relative_path`（而非 mod_id）。
- `.lang` 输出写 `key=value` 行；`.json` 写 JSON map（保留现有 pretty 输出）。
- 冲突检测：同一输出路径同一 key、文本不同 → 记录 `ConflictInfo`（新增 `path` 字段，`mod_id` 保留），后者覆盖前者。
- `pack.mcmeta` 描述统计按输出文件数。

`commands/pack.rs`：
- `generate_pack_from_job` 增加参数 `allow_partial: bool`；`allow_partial == true` 时跳过 `failed_entries > 0` 拦截（缺条目按未翻译处理，不写入资源包）。
- `PackEntry` 映射补 `source_file: r.source_file`；`source_type == "untranslated"` 或 `target_text.is_empty()` 的结果跳过。

### 3.5 导出 / 导入未翻译 JSON
`commands/localize.rs`（新文件，注册到 `commands/mod.rs` 与 `lib.rs`）：
- `export_untranslated(job_id, file_path) -> usize`：读 `_untranslated.jsonl`，输出：
```json
{ "schemaVersion": 1, "jobId": "...", "targetLanguage": "zh_cn",
  "entries": [ { "entryId": "...", "modId": "...", "sourceFile": "...", "key": "...", "sourceText": "...", "targetText": "" } ] }
```
- `import_untranslated_translations(job_id, file_path) -> ImportTranslationsResult { imported, skipped, invalid: Vec<InvalidTranslation> }`：
  1. 读 JSON（接受 `{entries:[...]}` 或裸数组）。
  2. 以 `_untranslated.jsonl` 建 `entryId -> UntranslatedEntry` 索引。
  3. 每条：`entryId` 必须存在（否则 `invalid`，原因 `unknownEntry`）；`targetText` 非空（`emptyTarget`）；占位符校验：`shield::protect(source)` 得到 tokens，`shield::validate(&tokens, target)` 必须通过（`placeholderMismatch`）。
  4. 通过者追加 `TranslationResult { source_type: "imported", source_file, .. }` 到结果 JSONL；结束时重写 `_untranslated.jsonl` 仅保留未导入项。
  5. **不打开、不写入任何词典数据库。**
- 命令 `discard_untranslated(job_id)`：清空 `_untranslated.jsonl`。

### 3.6 词典数据仓库
流水线已独立到 [xiaotang666/CraftLingo-Dictionary](https://github.com/xiaotang666/CraftLingo-Dictionary)，源码仓库不再保留副本：
- `build_dict.py`：合并自有汉化库 `dictionary/latest/`（目录内多文件，按名排序合并）与 VM `Dict.json`，VM 覆盖自有；生成 SQLite、`manifest.json`、`full-*.db.zst`、按上一版基线生成 `inc-*.sql.zst`；保留 10 个资产；`contentHash` 不变则退出码 0 且不发布。自有汉化库为 `xiaotang666/J20-Mcmodlanguage-Package`，构建时仅 sparse checkout `dictionary/latest`，`pending/` 由对方自动化处理。
- `.github/workflows/build-dict.yml`：`cron: '0 16 * * 0'`（UTC 周日 16:00 = 北京周一 00:00）+ `workflow_dispatch`；`permissions: contents: write`。
- 客户端默认 manifest：`https://cdn.jsdelivr.net/gh/xiaotang666/CraftLingo-Dictionary@main/manifest.json`；拉取失败时依次轮询国内镜像（`ghfast.top` / `gh-proxy.com` / `ghproxy.net` / `gh-proxy.net`）代理的 raw 地址，最后回退官方 raw。词典仅服务简中，故词典资源下载同样镜像优先、官方兜底。

### 3.7 前端
- `src/types.ts`：`Settings.i18nDictManifestUrl`；`TranslationResult.sourceFile?`；`UntranslatedExport`/`ImportTranslationsResult`；`PackResult.conflicts[].path?`。
- `src/api/tauri.ts`：`startAutoLocalize`、`exportUntranslated`、`importUntranslated`、`discardUntranslated`；`generatePackFromJob` 增 `allowPartial`。
- `src/pages/DashboardPage.tsx`：主按钮区增加「自动汉化」；完成后显示「词典命中 / 待翻译」；提供「导出待翻译」「导入译文」按钮（使用现有文件对话框插件）。
- `src/pages/SettingsPage.tsx`：i18n 词典卡片显示来源（`enhanced`/`official`）、当前/最新 tag、条目数；保留检查/更新按钮。
- `src/i18n/translations.ts`：新增 key 的五语言文案（zh_cn/en_us/ja_jp/ko_kr/ru_ru），类型联合同步。

## 4. 边界与错误处理
- 增量应用失败 → 自动回退全量，不破坏现有库（先写 `.next`，成功后原子替换）。
- 首次启动无词典：`active_i18n_dict_path` 返回明确错误，自动汉化仍可运行（仅本地词典，命中率低）。
- 导入文件缺字段/条目 → 计入 `invalid`，不中断其余条目。
- 同路径同 key 冲突 → 记入 `packer::ConflictInfo`，前端展示，不阻断打包。
- 取消：`start_auto_localize` 复用 `GLOBAL_CANCEL`。

## 5. 验证步骤
1. `cargo fmt --check`、`cargo clippy`（可容忍既有告警）。
2. `cargo test`（新增 `i18n_sync` 单测：链条解析、>3 全量、sha256、zstd 往返、SQL 增量；packer 路径/冲突/`.lang` 输出；localize 导入校验）。
3. `npm run test:unit`、`npm run build`。
4. 手工：扫描样例实例 → 自动汉化 → 校验 zip 内 `assets/<ns>/lang/zh_cn.json|.lang` 路径与命名空间一致；导出 → 外部填译 → 导入 → 生成包 → 词典条数不变。

## 6. 不做什么
- 不改原始 jar。
- 不在客户端做多源合并（VM 覆盖自有效力在服务端）。
- 不内置任何词典数据。
- 不自动定时构建客户端侧（周一 00:00 仅是服务端）。
- 导入不写入词典。

## 7. 待批准
请批准后开始实现（按 `06-dict-sync-localize-tasks.md` 顺序）。
