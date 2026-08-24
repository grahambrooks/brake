//! Hermetic document resolution for multi-file contract specifications.
//!
//! Supports local sibling reference resolution across files without network access,
//! maintaining strict hermetic execution and zero toolchain dependencies.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Trait for resolving sibling contract documents hermetically within repository boundaries.
///
/// Paths are relative to the **contract document's own directory**, never to
/// the repository root and never derived from the ingester's `source` label —
/// a baseline read out of git is labelled `rev:HEAD`, which names no directory
/// at all. A resolver is rooted where the document sits, so the directory is
/// also the boundary a `$ref` cannot reach past.
pub trait DocumentResolver: Send + Sync {
    /// Read a document path relative to the resolver's root.
    /// Never hits the network. Returns None if the document cannot be resolved.
    fn resolve(&self, relative_path: &str) -> Option<Vec<u8>>;
}

/// A resolver that refuses all external lookups.
/// Used for standalone string/byte slice parsing where no filesystem or repository context is provided.
#[derive(Debug, Default, Clone, Copy)]
pub struct SingleDocumentResolver;

impl DocumentResolver for SingleDocumentResolver {
    fn resolve(&self, _relative_path: &str) -> Option<Vec<u8>> {
        None
    }
}

/// An in-memory resolver that maps document paths to byte vectors.
/// Useful for unit tests, mocked multi-document bundles, and memory-based verification.
#[derive(Debug, Default, Clone)]
pub struct InMemoryResolver {
    documents: BTreeMap<String, Vec<u8>>,
}

impl InMemoryResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            documents: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_document(mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let p: String = path.into();
        let normalized = p.trim_start_matches("./").to_string();
        self.documents.insert(normalized, bytes.into());
        self
    }

    pub fn add_document(&mut self, path: impl Into<String>, bytes: impl Into<Vec<u8>>) {
        let p: String = path.into();
        let normalized = p.trim_start_matches("./").to_string();
        self.documents.insert(normalized, bytes.into());
    }
}

impl DocumentResolver for InMemoryResolver {
    fn resolve(&self, relative_path: &str) -> Option<Vec<u8>> {
        let normalized = relative_path.trim_start_matches("./");
        self.documents.get(normalized).cloned()
    }
}

/// A resolver that resolves sibling files from the local filesystem relative to a base directory.
/// Strictly enforces repository boundary checks and refuses absolute paths or upward escapes.
#[derive(Debug, Clone)]
pub struct FileSystemResolver {
    base_dir: PathBuf,
}

impl FileSystemResolver {
    #[must_use]
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }
}

impl DocumentResolver for FileSystemResolver {
    fn resolve(&self, relative_path: &str) -> Option<Vec<u8>> {
        let path = Path::new(relative_path);
        if path.is_absolute() {
            return None;
        }

        let target = self.base_dir.join(path);
        // Canonicalize or check path to prevent escaping base_dir
        if let Ok(canonical_target) = target.canonicalize()
            && let Ok(canonical_base) = self.base_dir.canonicalize()
            && !canonical_target.starts_with(&canonical_base)
        {
            return None;
        }

        std::fs::read(target).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn single_document_resolver_always_returns_none() {
        let resolver = SingleDocumentResolver;
        assert_eq!(resolver.resolve("common/types.yaml"), None);
    }

    #[test]
    fn in_memory_resolver_returns_stored_bytes() {
        let mut resolver = InMemoryResolver::new()
            .with_document("types.yaml", b"schema: content".to_vec())
            .with_document("./models/user.yaml", b"user: data".to_vec());

        resolver.add_document("./schemas/order.yaml", b"order: data".to_vec());

        assert_eq!(
            resolver.resolve("types.yaml"),
            Some(b"schema: content".to_vec())
        );
        assert_eq!(
            resolver.resolve("models/user.yaml"),
            Some(b"user: data".to_vec())
        );
        assert_eq!(
            resolver.resolve("./models/user.yaml"),
            Some(b"user: data".to_vec())
        );
        assert_eq!(
            resolver.resolve("schemas/order.yaml"),
            Some(b"order: data".to_vec())
        );
        assert_eq!(resolver.resolve("missing.yaml"), None);
    }

    #[test]
    fn filesystem_resolver_resolves_files_and_enforces_boundaries() {
        let dir = tempdir().expect("tempdir");
        let base_dir = dir.path().join("repo");
        std::fs::create_dir_all(&base_dir).expect("create_dir");

        let schema_file = base_dir.join("common.yaml");
        std::fs::write(&schema_file, b"schema: content").expect("write file");

        let outside_file = dir.path().join("outside.yaml");
        std::fs::write(&outside_file, b"secret").expect("write outside");

        let resolver = FileSystemResolver::new(&base_dir);

        // Valid relative resolution within boundary
        assert_eq!(
            resolver.resolve("common.yaml"),
            Some(b"schema: content".to_vec())
        );

        // Relative path traversal outside base directory is blocked
        assert_eq!(resolver.resolve("../outside.yaml"), None);

        // Absolute path is blocked
        assert_eq!(resolver.resolve("/etc/passwd"), None);

        // Missing file returns None
        assert_eq!(resolver.resolve("non_existent.yaml"), None);
    }
}
