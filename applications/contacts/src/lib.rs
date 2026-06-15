//! # Contacts
//!
//! A simple address book stored in the filesystem under `/contacts/<name>`, so
//! entries persist with the volume. Each contact maps a name to details (a
//! free-form line, e.g. a Gate number or note). Supports add, get, list,
//! remove, and search.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::Filesystem;

/// The contacts book over a filesystem.
pub struct Contacts {
    fs: Filesystem,
}

fn key(name: &str) -> String {
    format!("/contacts/{name}")
}

impl Contacts {
    /// Open the contacts book on `fs`.
    #[must_use]
    pub fn new(fs: Filesystem) -> Self {
        Contacts { fs }
    }

    /// Add or update a contact. Returns false if the name is unusable as a path.
    pub fn add(&self, name: &str, details: &str) -> bool {
        if name.is_empty() || name.contains(char::is_whitespace) {
            return false;
        }
        self.fs.write(&key(name), details.as_bytes())
    }

    /// Look up a contact's details.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<String> {
        self.fs
            .read(&key(name))
            .map(|b| String::from_utf8_lossy(&b).to_string())
    }

    /// Remove a contact; returns whether it existed.
    pub fn remove(&self, name: &str) -> bool {
        self.fs.delete(&key(name))
    }

    /// All contact names, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        // `list` returns immediate child names (the contract), so each entry is
        // already a bare contact name.
        let mut names = self.fs.list("/contacts/");
        names.sort();
        names
    }

    /// Names containing `query` (case-insensitive).
    #[must_use]
    pub fn search(&self, query: &str) -> Vec<String> {
        let q = query.to_lowercase();
        self.names()
            .into_iter()
            .filter(|n| n.to_lowercase().contains(&q))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_get_remove() {
        let c = Contacts::new(Filesystem::new());
        assert!(c.add("alice", "gate 5000"));
        assert!(c.add("bob", "gate 5001"));
        assert_eq!(c.get("alice").as_deref(), Some("gate 5000"));
        assert_eq!(c.names(), vec!["alice".to_string(), "bob".to_string()]);
        assert!(c.remove("alice"));
        assert_eq!(c.get("alice"), None);
    }

    #[test]
    fn rejects_bad_names_and_searches() {
        let c = Contacts::new(Filesystem::new());
        assert!(!c.add("has space", "x"));
        c.add("alice", "1");
        c.add("alex", "2");
        c.add("bob", "3");
        let mut hits = c.search("al");
        hits.sort();
        assert_eq!(hits, vec!["alex".to_string(), "alice".to_string()]);
    }
}
