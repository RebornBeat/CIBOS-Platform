//! # Filesystem
//!
//! A simple in-memory filesystem shared by all tasks of a [`System`](crate::System).
//! It is a flat, path-keyed store: paths are `/`-style strings, and listing by
//! prefix gives directory-like grouping without a separate tree structure. This
//! is the storage *service* CIBOS applications use through the SDK; like the
//! channel and spawn APIs, the on-device transport will differ but the surface
//! does not.
//!
//! It is deliberately minimal — read, write, delete, exists, list — and
//! synchronous, since the backing store is in memory. Concurrency is safe: all
//! tasks share one instance and access is serialized by a mutex.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// A shared in-memory filesystem handle. Cloning shares the same store.
#[derive(Clone, Default)]
pub struct Filesystem {
    inner: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
}

impl Filesystem {
    /// Create an empty filesystem.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `path` is a well-formed path: non-empty and containing no
    /// whitespace or quoting characters.
    #[must_use]
    pub fn is_valid_path(path: &str) -> bool {
        !path.is_empty()
            && !path.contains(char::is_whitespace)
            && !path.contains(['\'', '"'])
    }

    /// Write `data` to `path`, creating or replacing it. Returns `false` if the
    /// path is invalid.
    pub fn write(&self, path: &str, data: &[u8]) -> bool {
        if !Self::is_valid_path(path) {
            return false;
        }
        self.inner
            .lock()
            .unwrap()
            .insert(path.to_string(), data.to_vec());
        true
    }

    /// Read the contents of `path`, or `None` if it does not exist.
    #[must_use]
    pub fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.inner.lock().unwrap().get(path).cloned()
    }

    /// Delete `path`. Returns whether a file was removed.
    pub fn delete(&self, path: &str) -> bool {
        self.inner.lock().unwrap().remove(path).is_some()
    }

    /// Whether `path` exists.
    #[must_use]
    pub fn exists(&self, path: &str) -> bool {
        self.inner.lock().unwrap().contains_key(path)
    }

    /// All paths beginning with `prefix`, sorted. An empty prefix lists every
    /// path.
    #[must_use]
    pub fn list(&self, prefix: &str) -> Vec<String> {
        self.inner
            .lock()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect()
    }

    /// Number of files stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    /// Whether the filesystem is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_delete() {
        let fs = Filesystem::new();
        assert!(fs.is_empty());
        assert!(fs.write("/notes/todo.txt", b"buy milk"));
        assert!(fs.exists("/notes/todo.txt"));
        assert_eq!(fs.read("/notes/todo.txt").as_deref(), Some(&b"buy milk"[..]));
        assert_eq!(fs.len(), 1);
        assert!(fs.delete("/notes/todo.txt"));
        assert!(!fs.exists("/notes/todo.txt"));
        assert!(fs.read("/notes/todo.txt").is_none());
    }

    #[test]
    fn list_by_prefix() {
        let fs = Filesystem::new();
        fs.write("/a/one", b"1");
        fs.write("/a/two", b"2");
        fs.write("/b/three", b"3");
        let under_a = fs.list("/a/");
        assert_eq!(under_a, vec!["/a/one".to_string(), "/a/two".to_string()]);
        assert_eq!(fs.list("").len(), 3);
    }

    #[test]
    fn rejects_invalid_paths() {
        let fs = Filesystem::new();
        assert!(!fs.write("", b"x"));
        assert!(!fs.write("has space", b"x"));
        assert!(!fs.write("has\"quote", b"x"));
        assert!(fs.is_empty());
    }

    #[test]
    fn shared_handles_see_same_store() {
        let fs = Filesystem::new();
        let other = fs.clone();
        fs.write("/k", b"v");
        // A clone observes the write — this is the shared-service property.
        assert_eq!(other.read("/k").as_deref(), Some(&b"v"[..]));
    }
}
