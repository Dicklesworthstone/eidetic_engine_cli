use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::db::{DbConnection, DbError};
use crate::search::{
    CanonicalSearchDocument, EmbedderStack, HashEmbedder, IndexBuilder, memory_to_document,
    session_to_document,
};

pub const DEFAULT_INDEX_SUBDIR: &str = "index";

#[derive(Clone, Debug)]
pub struct IndexRebuildOptions {
    pub workspace_path: PathBuf,
    pub database_path: Option<PathBuf>,
    pub index_dir: Option<PathBuf>,
    pub dry_run: bool,
}

impl IndexRebuildOptions {
    fn resolve_database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join("ee.db"))
    }

    fn resolve_index_dir(&self) -> PathBuf {
        self.index_dir
            .clone()
            .unwrap_or_else(|| self.workspace_path.join(".ee").join(DEFAULT_INDEX_SUBDIR))
    }
}

#[derive(Clone, Debug)]
pub struct IndexRebuildReport {
    pub status: IndexRebuildStatus,
    pub memories_indexed: u32,
    pub sessions_indexed: u32,
    pub documents_total: u32,
    pub index_dir: PathBuf,
    pub elapsed_ms: f64,
    pub dry_run: bool,
    pub errors: Vec<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexRebuildStatus {
    Success,
    DryRun,
    NoDocuments,
    DatabaseError,
    IndexError,
}

impl IndexRebuildStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::DryRun => "dry_run",
            Self::NoDocuments => "no_documents",
            Self::DatabaseError => "database_error",
            Self::IndexError => "index_error",
        }
    }
}

impl IndexRebuildReport {
    #[must_use]
    pub fn human_summary(&self) -> String {
        let mut output = String::new();

        match self.status {
            IndexRebuildStatus::DryRun => {
                output.push_str("DRY RUN: Would rebuild search index\n\n");
            }
            IndexRebuildStatus::Success => {
                output.push_str("Search index rebuilt successfully\n\n");
            }
            IndexRebuildStatus::NoDocuments => {
                output.push_str("No documents to index\n\n");
            }
            IndexRebuildStatus::DatabaseError => {
                output.push_str("Database error during index rebuild\n\n");
            }
            IndexRebuildStatus::IndexError => {
                output.push_str("Index error during rebuild\n\n");
            }
        }

        output.push_str(&format!("  Memories: {}\n", self.memories_indexed));
        output.push_str(&format!("  Sessions: {}\n", self.sessions_indexed));
        output.push_str(&format!("  Total documents: {}\n", self.documents_total));
        output.push_str(&format!(
            "  Index directory: {}\n",
            self.index_dir.display()
        ));
        output.push_str(&format!("  Elapsed: {:.1}ms\n", self.elapsed_ms));

        if !self.errors.is_empty() {
            output.push_str("\nErrors:\n");
            for error in &self.errors {
                output.push_str(&format!("  - {error}\n"));
            }
        }

        output
    }

    #[must_use]
    pub fn data_json(&self) -> serde_json::Value {
        serde_json::json!({
            "command": "index_rebuild",
            "status": self.status.as_str(),
            "memories_indexed": self.memories_indexed,
            "sessions_indexed": self.sessions_indexed,
            "documents_total": self.documents_total,
            "index_dir": self.index_dir.to_string_lossy(),
            "elapsed_ms": self.elapsed_ms,
            "dry_run": self.dry_run,
            "errors": self.errors,
        })
    }
}

#[derive(Debug)]
pub enum IndexRebuildError {
    Database(DbError),
    Index(String),
    NoWorkspace,
}

impl IndexRebuildError {
    #[must_use]
    pub fn repair_hint(&self) -> Option<&str> {
        match self {
            Self::Database(_) => Some("ee doctor --fix-plan --json"),
            Self::Index(_) => Some("Check index directory permissions"),
            Self::NoWorkspace => Some("ee init --workspace ."),
        }
    }
}

impl std::fmt::Display for IndexRebuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Database(e) => write!(f, "Database error: {e}"),
            Self::Index(e) => write!(f, "Index error: {e}"),
            Self::NoWorkspace => write!(f, "No workspace found"),
        }
    }
}

impl std::error::Error for IndexRebuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Database(e) => Some(e),
            _ => None,
        }
    }
}

impl From<DbError> for IndexRebuildError {
    fn from(e: DbError) -> Self {
        Self::Database(e)
    }
}

pub fn rebuild_index(
    options: &IndexRebuildOptions,
) -> Result<IndexRebuildReport, IndexRebuildError> {
    let start = Instant::now();
    let database_path = options.resolve_database_path();
    let index_dir = options.resolve_index_dir();

    let db = DbConnection::open_file(&database_path)?;

    let workspace_id = get_default_workspace_id(&db)?;

    let memories = db.list_memories(&workspace_id, None, false)?;
    let sessions = db.list_sessions(&workspace_id)?;

    let memory_docs: Vec<CanonicalSearchDocument> =
        memories.iter().map(memory_to_document).collect();
    let session_docs: Vec<CanonicalSearchDocument> =
        sessions.iter().map(session_to_document).collect();

    let memories_indexed = memory_docs.len() as u32;
    let sessions_indexed = session_docs.len() as u32;
    let documents_total = memories_indexed + sessions_indexed;

    if options.dry_run {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexRebuildReport {
            status: IndexRebuildStatus::DryRun,
            memories_indexed,
            sessions_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: true,
            errors: Vec::new(),
        });
    }

    if documents_total == 0 {
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        return Ok(IndexRebuildReport {
            status: IndexRebuildStatus::NoDocuments,
            memories_indexed: 0,
            sessions_indexed: 0,
            documents_total: 0,
            index_dir,
            elapsed_ms,
            dry_run: false,
            errors: Vec::new(),
        });
    }

    std::fs::create_dir_all(&index_dir)
        .map_err(|e| IndexRebuildError::Index(format!("Failed to create index directory: {e}")))?;

    let indexable_docs: Vec<_> = memory_docs
        .into_iter()
        .chain(session_docs)
        .map(|doc| doc.into_indexable())
        .collect();

    let fast_embedder = Arc::new(HashEmbedder::default_256()) as Arc<dyn crate::search::Embedder>;
    let quality_embedder =
        Arc::new(HashEmbedder::default_384()) as Arc<dyn crate::search::Embedder>;
    let stack = EmbedderStack::from_parts(fast_embedder, Some(quality_embedder));

    let build_result = build_index_sync(&index_dir, stack, indexable_docs);

    let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;

    match build_result {
        Ok(stats) => Ok(IndexRebuildReport {
            status: IndexRebuildStatus::Success,
            memories_indexed,
            sessions_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: false,
            errors: stats
                .errors
                .iter()
                .map(|(id, e)| format!("{id}: {e}"))
                .collect(),
        }),
        Err(e) => Ok(IndexRebuildReport {
            status: IndexRebuildStatus::IndexError,
            memories_indexed,
            sessions_indexed,
            documents_total,
            index_dir,
            elapsed_ms,
            dry_run: false,
            errors: vec![e],
        }),
    }
}

fn get_default_workspace_id(db: &DbConnection) -> Result<String, IndexRebuildError> {
    let rows = db.query(
        "SELECT id FROM workspaces ORDER BY created_at DESC LIMIT 1",
        &[],
    )?;

    rows.first()
        .and_then(|row| row.get(0).and_then(|v| v.as_str().map(str::to_string)))
        .ok_or(IndexRebuildError::NoWorkspace)
}

struct BuildStats {
    #[expect(dead_code)]
    doc_count: usize,
    errors: Vec<(String, String)>,
}

fn build_index_sync(
    index_dir: &Path,
    stack: EmbedderStack,
    documents: Vec<crate::search::IndexableDocument>,
) -> Result<BuildStats, String> {
    use asupersync::test_utils::run_test_with_cx;

    let index_dir_owned = index_dir.to_path_buf();
    let result_holder: Arc<Mutex<Option<Result<BuildStats, String>>>> = Arc::new(Mutex::new(None));
    let result_clone = Arc::clone(&result_holder);

    let panic_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_test_with_cx(|cx| async move {
            let mut builder = IndexBuilder::new(&index_dir_owned).with_embedder_stack(stack);

            for doc in documents {
                builder = builder.add_document(doc.id.clone(), doc.content.clone());
            }

            let build_result = builder.build(&cx).await;
            let converted = match build_result {
                Ok(stats) => Ok(BuildStats {
                    doc_count: stats.doc_count,
                    errors: stats.errors,
                }),
                Err(e) => Err(format!("Index build failed: {e}")),
            };
            if let Ok(mut guard) = result_clone.lock() {
                *guard = Some(converted);
            }
        })
    }));

    match panic_result {
        Ok(()) => result_holder
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
            .unwrap_or_else(|| Err("Index build result not captured".to_string())),
        Err(_) => Err("Index build panicked".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_rebuild_status_as_str_is_stable() {
        assert_eq!(IndexRebuildStatus::Success.as_str(), "success");
        assert_eq!(IndexRebuildStatus::DryRun.as_str(), "dry_run");
        assert_eq!(IndexRebuildStatus::NoDocuments.as_str(), "no_documents");
        assert_eq!(IndexRebuildStatus::DatabaseError.as_str(), "database_error");
        assert_eq!(IndexRebuildStatus::IndexError.as_str(), "index_error");
    }

    #[test]
    fn index_rebuild_report_data_json_has_required_fields() {
        let report = IndexRebuildReport {
            status: IndexRebuildStatus::Success,
            memories_indexed: 5,
            sessions_indexed: 3,
            documents_total: 8,
            index_dir: PathBuf::from("/tmp/index"),
            elapsed_ms: 123.4,
            dry_run: false,
            errors: Vec::new(),
        };

        let json = report.data_json();
        assert_eq!(json["command"], "index_rebuild");
        assert_eq!(json["status"], "success");
        assert_eq!(json["memories_indexed"], 5);
        assert_eq!(json["sessions_indexed"], 3);
        assert_eq!(json["documents_total"], 8);
        assert_eq!(json["dry_run"], false);
    }

    #[test]
    fn index_rebuild_options_resolve_paths() {
        let options = IndexRebuildOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: None,
            index_dir: None,
            dry_run: false,
        };

        assert_eq!(
            options.resolve_database_path(),
            PathBuf::from("/home/user/project/.ee/ee.db")
        );
        assert_eq!(
            options.resolve_index_dir(),
            PathBuf::from("/home/user/project/.ee/index")
        );
    }

    #[test]
    fn index_rebuild_options_respect_explicit_paths() {
        let options = IndexRebuildOptions {
            workspace_path: PathBuf::from("/home/user/project"),
            database_path: Some(PathBuf::from("/custom/db.sqlite")),
            index_dir: Some(PathBuf::from("/custom/index")),
            dry_run: true,
        };

        assert_eq!(
            options.resolve_database_path(),
            PathBuf::from("/custom/db.sqlite")
        );
        assert_eq!(options.resolve_index_dir(), PathBuf::from("/custom/index"));
    }
}
