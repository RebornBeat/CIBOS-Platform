//! # Storage: Live and Persistent volumes
//!
//! CIBOS is designed to run from a USB stick in either of two modes, natively:
//!
//! * **Live** — the working filesystem lives entirely in RAM. On shutdown it is
//!   zeroized and dropped, leaving **no trace** on any persistent medium. Live
//!   mode never reads or writes a persistence partition, even if one exists.
//! * **Persistent** — the working filesystem is backed by a **persistence
//!   partition**. On boot its contents are loaded into RAM; on `sync`/shutdown
//!   the RAM contents are written back, so files and settings survive reboots.
//!
//! The partition is modeled here as a raw byte region (a [`PersistenceStore`]),
//! exactly as a real block partition would be — the working filesystem is
//! serialized to / deserialized from a single partition image. The in-RAM
//! working set is the SDK [`Filesystem`], so applications use storage the same
//! way in both modes; only persistence differs.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use cibos_sdk::Filesystem;
use std::sync::{Arc, Mutex};

/// Which persistence mode a volume is mounted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceMode {
    /// RAM-only; wiped on shutdown, never touches a partition.
    Live,
    /// Backed by a persistence partition; survives reboots.
    Persistent,
}

/// Serialize filesystem entries into a single partition image. Format: for each
/// entry, `u32` path length, path bytes, `u32` data length, data bytes (all
/// little-endian).
#[must_use]
pub fn serialize_entries(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (path, data) in entries {
        out.extend_from_slice(&(path.len() as u32).to_le_bytes());
        out.extend_from_slice(path.as_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
    }
    out
}

/// Deserialize a partition image back into entries. Malformed/truncated trailing
/// bytes are ignored (a freshly-wiped, all-zero region yields no entries).
#[must_use]
pub fn deserialize_entries(blob: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    let mut i = 0usize;
    while i + 4 <= blob.len() {
        let plen = u32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]) as usize;
        i += 4;
        // A zero-length path with no following data marks the end (or empty
        // region); stop cleanly.
        if plen == 0 || i + plen + 4 > blob.len() {
            break;
        }
        let path = match std::str::from_utf8(&blob[i..i + plen]) {
            Ok(s) => s.to_string(),
            Err(_) => break,
        };
        i += plen;
        let dlen = u32::from_le_bytes([blob[i], blob[i + 1], blob[i + 2], blob[i + 3]]) as usize;
        i += 4;
        if i + dlen > blob.len() {
            break;
        }
        let data = blob[i..i + dlen].to_vec();
        i += dlen;
        entries.push((path, data));
    }
    entries
}

/// A persistence backend — the raw partition. Implementations store and return
/// a single image blob.
pub trait PersistenceStore: Send + Sync {
    /// Read the current partition image.
    fn read_image(&self) -> Vec<u8>;
    /// Replace the partition image.
    fn write_image(&self, image: &[u8]);
    /// Zero the partition.
    fn wipe(&self);
}

/// An in-memory persistence partition (a raw byte region), standing in for a USB
/// persistence partition.
#[derive(Clone, Default)]
pub struct MemoryPartition {
    image: Arc<Mutex<Vec<u8>>>,
}

impl MemoryPartition {
    /// A new, empty partition.
    #[must_use]
    pub fn new() -> Self {
        MemoryPartition::default()
    }

    /// The current image size in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.image.lock().unwrap().len()
    }

    /// Whether the partition image is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.image.lock().unwrap().is_empty()
    }
}

impl PersistenceStore for MemoryPartition {
    fn read_image(&self) -> Vec<u8> {
        self.image.lock().unwrap().clone()
    }
    fn write_image(&self, image: &[u8]) {
        *self.image.lock().unwrap() = image.to_vec();
    }
    fn wipe(&self) {
        let mut g = self.image.lock().unwrap();
        // Zeroize, then drop, so no trace remains.
        for b in g.iter_mut() {
            *b = 0;
        }
        g.clear();
    }
}

/// A mounted volume: the in-RAM working filesystem plus its mode and (for
/// Persistent) the backing partition.
pub struct Volume {
    fs: Filesystem,
    mode: PersistenceMode,
    store: Option<Arc<dyn PersistenceStore>>,
}

impl Volume {
    /// Boot a volume in `mode`. The machine's `partition` is consulted only in
    /// Persistent mode — in Live mode it is neither read nor retained, so the
    /// session cannot touch it.
    #[must_use]
    pub fn boot(mode: PersistenceMode, partition: Arc<dyn PersistenceStore>) -> Self {
        let fs = Filesystem::new();
        match mode {
            PersistenceMode::Persistent => {
                for (path, data) in deserialize_entries(&partition.read_image()) {
                    fs.write(&path, &data);
                }
                Volume {
                    fs,
                    mode,
                    store: Some(partition),
                }
            }
            PersistenceMode::Live => Volume {
                fs,
                mode,
                store: None, // deliberately drop the partition reference
            },
        }
    }

    /// Boot a Live volume with no partition at all.
    #[must_use]
    pub fn live() -> Self {
        Volume {
            fs: Filesystem::new(),
            mode: PersistenceMode::Live,
            store: None,
        }
    }

    /// The working filesystem applications use.
    #[must_use]
    pub fn filesystem(&self) -> Filesystem {
        self.fs.clone()
    }

    /// The mount mode.
    #[must_use]
    pub fn mode(&self) -> PersistenceMode {
        self.mode
    }

    /// Snapshot the working filesystem into sorted `(path, bytes)` entries.
    fn snapshot(&self) -> Vec<(String, Vec<u8>)> {
        let mut entries: Vec<(String, Vec<u8>)> = self
            .fs
            .list("")
            .into_iter()
            .filter_map(|p| self.fs.read(&p).map(|d| (p, d)))
            .collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        entries
    }

    /// Persist the current working set to the partition. No-op in Live mode.
    pub fn sync(&self) {
        if let Some(store) = &self.store {
            let image = serialize_entries(&self.snapshot());
            store.write_image(&image);
        }
    }

    /// Zeroize the in-RAM working filesystem (no trace in memory).
    fn wipe_ram(&self) {
        for path in self.fs.list("") {
            // Overwrite with zeros of equal length before removing.
            if let Some(data) = self.fs.read(&path) {
                self.fs.write(&path, &vec![0u8; data.len()]);
            }
            self.fs.delete(&path);
        }
    }

    /// Shut the volume down.
    ///
    /// * Persistent: commit the working set to the partition, then wipe RAM.
    /// * Live: wipe RAM only. The partition (if the machine has one) is never
    ///   written, so the session leaves no trace.
    pub fn shutdown(self) {
        if self.mode == PersistenceMode::Persistent {
            self.sync();
        }
        self.wipe_ram();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_round_trip() {
        let entries = vec![
            ("/a".to_string(), b"alpha".to_vec()),
            ("/b/c".to_string(), vec![0u8, 1, 2, 255]),
        ];
        let blob = serialize_entries(&entries);
        assert_eq!(deserialize_entries(&blob), entries);
        // An all-zero region (freshly wiped) yields nothing.
        assert!(deserialize_entries(&[0u8; 64]).is_empty());
    }

    #[test]
    fn persistent_survives_reboot() {
        let partition: Arc<dyn PersistenceStore> = Arc::new(MemoryPartition::new());

        // First boot: write some files and a setting, then shut down.
        let vol = Volume::boot(PersistenceMode::Persistent, partition.clone());
        let fs = vol.filesystem();
        fs.write("/docs/note.txt", b"remember this");
        fs.write("/settings/theme", b"dark");
        vol.shutdown(); // commits to the partition, wipes RAM

        // The partition now holds an image.
        // Second boot from the same partition: state is restored.
        let vol2 = Volume::boot(PersistenceMode::Persistent, partition.clone());
        let fs2 = vol2.filesystem();
        assert_eq!(fs2.read("/docs/note.txt").as_deref(), Some(&b"remember this"[..]));
        assert_eq!(fs2.read("/settings/theme").as_deref(), Some(&b"dark"[..]));
    }

    #[test]
    fn live_leaves_no_trace_on_partition() {
        // The machine has a partition with prior persistent data.
        let partition = Arc::new(MemoryPartition::new());
        {
            let vol = Volume::boot(PersistenceMode::Persistent, partition.clone());
            vol.filesystem().write("/old", b"persistent data");
            vol.shutdown();
        }
        let image_before = partition.read_image();
        assert!(!image_before.is_empty());

        // Now boot LIVE on the same machine.
        let live = Volume::boot(PersistenceMode::Live, partition.clone());
        // Live does not load the partition: the working set starts empty.
        assert!(live.filesystem().list("").is_empty());
        // Do work in the live session.
        live.filesystem().write("/secret", b"ephemeral");
        live.sync(); // no-op in Live mode
        live.shutdown();

        // The partition is byte-for-byte unchanged: the live session left no
        // trace on it.
        assert_eq!(partition.read_image(), image_before);
    }

    #[test]
    fn live_wipes_ram_on_shutdown() {
        let live = Volume::live();
        let fs = live.filesystem();
        fs.write("/tmp/scratch", b"sensitive");
        assert!(fs.exists("/tmp/scratch"));
        live.shutdown();
        // The shared handle observes the wipe: nothing remains.
        assert!(fs.list("").is_empty());
    }

    #[test]
    fn persistent_sync_without_shutdown() {
        let partition: Arc<dyn PersistenceStore> = Arc::new(MemoryPartition::new());
        let vol = Volume::boot(PersistenceMode::Persistent, partition.clone());
        vol.filesystem().write("/k", b"v1");
        vol.sync();
        // Reboot sees the synced value even though the first volume is still up.
        let vol2 = Volume::boot(PersistenceMode::Persistent, partition);
        assert_eq!(vol2.filesystem().read("/k").as_deref(), Some(&b"v1"[..]));
    }

    #[test]
    fn wipe_clears_partition() {
        let partition = MemoryPartition::new();
        partition.write_image(&serialize_entries(&[("/x".to_string(), b"y".to_vec())]));
        assert!(!partition.is_empty());
        partition.wipe();
        assert!(partition.is_empty());
        assert!(deserialize_entries(&partition.read_image()).is_empty());
    }
}
