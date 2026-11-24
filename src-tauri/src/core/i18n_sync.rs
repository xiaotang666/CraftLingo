//! Client-side helpers for the enhanced i18n dictionary data repository.
//!
//! The data repository publishes a `manifest.json` plus one full database and a
//! set of per-baseline incremental SQL scripts. See
//! `docs/06-dict-sync-localize-spec.md` for the artifact format.

use std::io::{self, Read, Write};
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Maximum number of releases we are willing to catch up incrementally.
/// Being further behind than this triggers a full download.
pub const MAX_INCREMENT_STEPS: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct Manifest {
    pub schema_version: u32,
    pub tag: String,
    pub name: String,
    pub published_at: String,
    pub entry_count: usize,
    pub content_hash: String,
    pub full: Option<ManifestAsset>,
    pub increments: Vec<ManifestIncrement>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ManifestAsset {
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub compression: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ManifestIncrement {
    /// Tag of the database this increment must be applied on top of.
    pub from_tag: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    pub compression: String,
}

/// How the client should reach the latest dictionary tag.
#[derive(Debug, Clone, PartialEq)]
pub enum ChainPlan {
    /// Download the full database. `reason` is for logging/UI.
    Full { reason: String },
    /// Apply these increments in order (currently at most one hop).
    Incremental(Vec<ManifestIncrement>),
}

/// Decide whether the local tag can be updated incrementally.
///
/// `increments` is ordered newest-first in the manifest. If the local tag is
/// found within the first `MAX_INCREMENT_STEPS` baselines, a single hop to the
/// latest tag is enough. Otherwise (unknown tag, chain broken, or too far
/// behind) the caller must download the full database.
pub fn resolve_chain(manifest: &Manifest, current_tag: Option<&str>) -> ChainPlan {
    let Some(current) = current_tag.filter(|tag| !tag.trim().is_empty()) else {
        return ChainPlan::Full {
            reason: "no local dictionary tag".to_string(),
        };
    };
    if current == manifest.tag {
        return ChainPlan::Full {
            reason: "already up to date".to_string(),
        };
    }
    if let Some(position) = manifest
        .increments
        .iter()
        .position(|increment| increment.from_tag == current)
    {
        if position < MAX_INCREMENT_STEPS {
            return ChainPlan::Incremental(vec![manifest.increments[position].clone()]);
        }
        return ChainPlan::Full {
            reason: format!(
                "local tag is {} releases behind (> {MAX_INCREMENT_STEPS})",
                position + 1
            ),
        };
    }
    ChainPlan::Full {
        reason: format!("no increment published for local tag {current}"),
    }
}

/// Compute the lowercase hex SHA-256 of a file, streaming so memory stays flat.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Decompress a zstd file from `source` into `dest`.
pub fn zstd_decompress_file(source: &Path, dest: &Path) -> io::Result<()> {
    let input = std::fs::File::open(source)?;
    let mut decoder = zstd::stream::read::Decoder::new(input)?;
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = std::fs::File::create(dest)?;
    io::copy(&mut decoder, &mut output)?;
    output.flush()?;
    Ok(())
}

/// Apply an incremental SQL script inside a single transaction.
///
/// The script must contain bare statements (no BEGIN/COMMIT).
pub fn apply_increment_sql(conn: &Connection, sql: &str) -> rusqlite::Result<()> {
    conn.execute_batch("BEGIN IMMEDIATE;")?;
    match conn.execute_batch(sql) {
        Ok(()) => conn.execute_batch("COMMIT;"),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK;");
            Err(error)
        }
    }
}

/// Verify that `path` is a usable reference dictionary and return its entry count.
pub fn validate_reference_db(path: &Path) -> Result<usize, String> {
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("打开参考词典失败: {e}"))?;
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dict WHERE TRIM(ORIGIN_NAME) <> '' AND TRIM(TRANS_NAME) <> ''",
            [],
            |row| row.get(0),
        )
        .map_err(|e| format!("校验参考词典失败（缺少 dict 表？）: {e}"))?;
    usize::try_from(count).map_err(|e| format!("参考词条数异常: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with(tag: &str, baselines: &[&str]) -> Manifest {
        Manifest {
            schema_version: 1,
            tag: tag.to_string(),
            increments: baselines
                .iter()
                .map(|from| ManifestIncrement {
                    from_tag: (*from).to_string(),
                    url: format!("inc-{from}-{tag}.sql.zst"),
                    sha256: String::new(),
                    size: 0,
                    compression: "zstd".to_string(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn missing_tag_forces_full_download() {
        let manifest = manifest_with("2026-11-23", &["2026-11-16"]);
        assert!(matches!(
            resolve_chain(&manifest, None),
            ChainPlan::Full { .. }
        ));
    }

    #[test]
    fn recent_baseline_uses_incremental() {
        let manifest = manifest_with(
            "2026-11-23",
            &["2026-11-16", "2026-11-09", "2026-11-02", "2026-10-26"],
        );
        match resolve_chain(&manifest, Some("2026-11-09")) {
            ChainPlan::Incremental(chain) => {
                assert_eq!(chain.len(), 1);
                assert_eq!(chain[0].from_tag, "2026-11-09");
            }
            other => panic!("expected incremental, got {other:?}"),
        }
    }

    #[test]
    fn more_than_three_releases_behind_forces_full() {
        let manifest = manifest_with(
            "2026-11-23",
            &["2026-11-16", "2026-11-09", "2026-11-02", "2026-10-26"],
        );
        assert!(matches!(
            resolve_chain(&manifest, Some("2026-10-26")),
            ChainPlan::Full { .. }
        ));
    }

    #[test]
    fn unknown_baseline_forces_full() {
        let manifest = manifest_with("2026-11-23", &["2026-11-16"]);
        assert!(matches!(
            resolve_chain(&manifest, Some("2025-01-01")),
            ChainPlan::Full { .. }
        ));
    }

    #[test]
    fn sha256_matches_known_vector() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("abc.txt");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn zstd_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("data.sql");
        let packed = dir.path().join("data.sql.zst");
        let out = dir.path().join("out.sql");
        std::fs::write(&src, b"INSERT INTO dict(ORIGIN_NAME) VALUES('x');\n").unwrap();
        {
            let mut encoder =
                zstd::stream::write::Encoder::new(std::fs::File::create(&packed).unwrap(), 1)
                    .unwrap();
            io::copy(&mut std::fs::File::open(&src).unwrap(), &mut encoder).unwrap();
            encoder.finish().unwrap();
        }
        zstd_decompress_file(&packed, &out).unwrap();
        assert_eq!(std::fs::read(&out).unwrap(), std::fs::read(&src).unwrap());
    }

    #[test]
    fn increment_sql_rolls_back_on_error() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE dict(ID INTEGER PRIMARY KEY, ORIGIN_NAME TEXT, TRANS_NAME TEXT);
             INSERT INTO dict VALUES(1, 'keep', 'keep');",
        )
        .unwrap();
        let outcome = apply_increment_sql(
            &conn,
            "INSERT INTO dict VALUES(2, 'new', 'new'); INSERT INTO missing_table VALUES(1);",
        );
        assert!(outcome.is_err());
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM dict", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1, "transaction must roll back");
    }
}
