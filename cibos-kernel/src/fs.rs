//! # CIBOSFS v1 — a small on-disk filesystem
//!
//! A real, block-backed filesystem layered on the [`BlockDevice`] trait, so it
//! works identically over the bare-metal ATA driver and an in-memory test
//! device. It is deliberately simple but genuinely persistent: files and
//! directories live in inodes and data blocks on the medium and survive a
//! reboot — which is what user accounts, credentials, and configuration need.
//!
//! ## On-disk layout (512-byte blocks)
//!
//! ```text
//!   block 0            superblock
//!   bitmap_start ..    block allocation bitmap (1 bit per data block)
//!   inode_start  ..    inode table (INODE_SIZE bytes each)
//!   data_start   ..    data blocks
//! ```
//!
//! ## Inodes
//!
//! Each inode is [`INODE_SIZE`] bytes: a kind (free / file / directory), a byte
//! size, and [`DIRECT_BLOCKS`] direct block pointers (no indirect blocks yet, so
//! a single file is bounded by `DIRECT_BLOCKS * BLOCK_SIZE`). Inode 0 is the
//! root directory, created by [`Fs::format`].
//!
//! ## Directories
//!
//! A directory is an inode whose data is a packed array of fixed-size entries
//! ([`DIR_ENTRY_SIZE`] bytes: a u32 inode number, a u8 name length, and a
//! [`NAME_MAX`]-byte name field). A free entry has inode number 0. Paths are
//! resolved component-by-component from the root.

use crate::block::{BlockDevice, BlockError, BLOCK_SIZE};
use alloc::vec;
use alloc::vec::Vec;
use shared::types::error::SerializationError;
use shared::utils::serialization::{ByteReader, ByteWriter};

/// Filesystem magic: ASCII `"CIBOSFS1"`, little-endian.
pub const FS_MAGIC: u64 = u64::from_le_bytes(*b"CIBOSFS1");
/// Filesystem version.
pub const FS_VERSION: u32 = 1;

/// Bytes per inode on disk.
pub const INODE_SIZE: usize = 64;
/// Direct block pointers per inode. With a 64-byte inode: kind(4) + reserved(4)
/// + size(8) + 6 × u64 pointers (48) = 64.
pub const DIRECT_BLOCKS: usize = 6;
/// Maximum file/dir size in bytes (direct blocks only).
pub const MAX_FILE_SIZE: u64 = (DIRECT_BLOCKS * BLOCK_SIZE) as u64;

/// Maximum directory-entry name length.
pub const NAME_MAX: usize = 23;
/// Bytes per directory entry on disk: inode(4) + name_len(1) + name(23) + pad(4)
/// = 32, so 16 entries fit per 512-byte block.
pub const DIR_ENTRY_SIZE: usize = 32;

/// Inode number of the root directory.
pub const ROOT_INODE: u32 = 0;

/// Inode kind tags (stored as u32).
const KIND_FREE: u32 = 0;
const KIND_FILE: u32 = 1;
const KIND_DIR: u32 = 2;

/// A filesystem error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    /// Underlying block device error.
    Block(BlockError),
    /// Superblock magic/version mismatch (not a CIBOSFS volume).
    BadSuperblock,
    /// On-disk structure failed to decode.
    Corrupt,
    /// No free inode / no free data block.
    NoSpace,
    /// A path component was not found.
    NotFound,
    /// A name already exists in the directory.
    Exists,
    /// The inode is not of the expected kind (e.g. not a directory).
    WrongKind,
    /// A name exceeded [`NAME_MAX`], or a write exceeded [`MAX_FILE_SIZE`].
    TooLarge,
}

impl From<BlockError> for FsError {
    fn from(e: BlockError) -> Self {
        FsError::Block(e)
    }
}
impl From<SerializationError> for FsError {
    fn from(_: SerializationError) -> Self {
        FsError::Corrupt
    }
}

/// In-memory copy of the superblock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Superblock {
    /// Total blocks on the volume.
    pub total_blocks: u64,
    /// First block of the allocation bitmap.
    pub bitmap_start: u64,
    /// Number of bitmap blocks.
    pub bitmap_blocks: u64,
    /// First block of the inode table.
    pub inode_start: u64,
    /// Number of inodes.
    pub inode_count: u32,
    /// First data block.
    pub data_start: u64,
    /// Number of data blocks (bitmap covers exactly these).
    pub data_blocks: u64,
}

impl Superblock {
    fn write_to(&self, buf: &mut [u8]) -> Result<(), FsError> {
        let mut w = ByteWriter::new(buf);
        w.put_u64(FS_MAGIC)?;
        w.put_u32(FS_VERSION)?;
        w.put_u32(self.inode_count)?;
        w.put_u64(self.total_blocks)?;
        w.put_u64(self.bitmap_start)?;
        w.put_u64(self.bitmap_blocks)?;
        w.put_u64(self.inode_start)?;
        w.put_u64(self.data_start)?;
        w.put_u64(self.data_blocks)?;
        Ok(())
    }

    fn read_from(buf: &[u8]) -> Result<Self, FsError> {
        let mut r = ByteReader::new(buf);
        let magic = r.get_u64()?;
        let version = r.get_u32()?;
        if magic != FS_MAGIC || version != FS_VERSION {
            return Err(FsError::BadSuperblock);
        }
        let inode_count = r.get_u32()?;
        Ok(Superblock {
            inode_count,
            total_blocks: r.get_u64()?,
            bitmap_start: r.get_u64()?,
            bitmap_blocks: r.get_u64()?,
            inode_start: r.get_u64()?,
            data_start: r.get_u64()?,
            data_blocks: r.get_u64()?,
        })
    }
}

/// An inode (file or directory).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Inode {
    kind: u32,
    size: u64,
    direct: [u64; DIRECT_BLOCKS],
}

impl Inode {
    fn write_to(&self, buf: &mut [u8]) -> Result<(), FsError> {
        let mut w = ByteWriter::new(buf);
        w.put_u32(self.kind)?;
        w.put_u32(0)?; // reserved (keeps size u64 aligned)
        w.put_u64(self.size)?;
        for p in &self.direct {
            w.put_u64(*p)?;
        }
        Ok(())
    }

    fn read_from(buf: &[u8]) -> Result<Self, FsError> {
        let mut r = ByteReader::new(buf);
        let kind = r.get_u32()?;
        let _reserved = r.get_u32()?;
        let size = r.get_u64()?;
        let mut direct = [0u64; DIRECT_BLOCKS];
        for p in direct.iter_mut() {
            *p = r.get_u64()?;
        }
        Ok(Inode { kind, size, direct })
    }

    fn blocks_used(&self) -> usize {
        (self.size as usize).div_ceil(BLOCK_SIZE)
    }
}

/// A directory entry as exposed to callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Inode number of the entry.
    pub inode: u32,
    /// Whether it is a directory.
    pub is_dir: bool,
    /// Entry name.
    pub name: Vec<u8>,
}

/// A mounted CIBOSFS filesystem over a block device `D`.
pub struct Fs<D: BlockDevice> {
    dev: D,
    sb: Superblock,
}

impl<D: BlockDevice> Fs<D> {
    /// Format `dev` as a fresh CIBOSFS volume with `inode_count` inodes, then
    /// return it mounted. Lays out superblock, bitmap, inode table, and data
    /// region, and creates an empty root directory at [`ROOT_INODE`].
    ///
    /// # Errors
    ///
    /// [`FsError`] on a block error or if the device is too small.
    pub fn format(dev: D, inode_count: u32) -> Result<Self, FsError> {
        let total = dev.block_count();
        let bitmap_start = 1u64;

        // Inode table size in blocks.
        let inodes_per_block = (BLOCK_SIZE / INODE_SIZE) as u64;
        let inode_blocks = (inode_count as u64).div_ceil(inodes_per_block);

        // Bitmap covers data blocks: 1 bit each => BLOCK_SIZE*8 data blocks per
        // bitmap block. Solve for the bitmap size iteratively (converges fast).
        let bits_per_block = (BLOCK_SIZE * 8) as u64;
        let fixed = bitmap_start; // blocks before the bitmap
        let mut bitmap_blocks = 1u64;
        loop {
            let overhead = fixed + bitmap_blocks + inode_blocks;
            if overhead >= total {
                return Err(FsError::NoSpace);
            }
            let data_blocks = total - overhead;
            let needed = data_blocks.div_ceil(bits_per_block).max(1);
            if needed <= bitmap_blocks {
                break;
            }
            bitmap_blocks = needed;
        }
        let inode_start = bitmap_start + bitmap_blocks;
        let data_start = inode_start + inode_blocks;
        let data_blocks = total - data_start;

        let sb = Superblock {
            total_blocks: total,
            bitmap_start,
            bitmap_blocks,
            inode_start,
            inode_count,
            data_start,
            data_blocks,
        };

        // Zero bitmap + inode table.
        let zero = vec![0u8; BLOCK_SIZE];
        for b in 0..bitmap_blocks {
            dev.write_blocks(bitmap_start + b, 1, &zero)?;
        }
        for b in 0..inode_blocks {
            dev.write_blocks(inode_start + b, 1, &zero)?;
        }

        // Write the superblock.
        let mut sbbuf = vec![0u8; BLOCK_SIZE];
        sb.write_to(&mut sbbuf)?;
        dev.write_blocks(0, 1, &sbbuf)?;

        let mut fs = Fs { dev, sb };

        // Create the root directory inode (empty).
        let root = Inode {
            kind: KIND_DIR,
            size: 0,
            direct: [0; DIRECT_BLOCKS],
        };
        fs.write_inode(ROOT_INODE, &root)?;

        Ok(fs)
    }

    /// Mount an existing CIBOSFS volume on `dev`.
    ///
    /// # Errors
    ///
    /// [`FsError::BadSuperblock`] if `dev` is not a CIBOSFS volume.
    pub fn mount(dev: D) -> Result<Self, FsError> {
        let mut sbbuf = vec![0u8; BLOCK_SIZE];
        dev.read_blocks(0, 1, &mut sbbuf)?;
        let sb = Superblock::read_from(&sbbuf)?;
        Ok(Fs { dev, sb })
    }

    /// The superblock.
    #[must_use]
    pub fn superblock(&self) -> &Superblock {
        &self.sb
    }

    /// Consume the filesystem and return the underlying device.
    pub fn into_device(self) -> D {
        self.dev
    }

    // ---- inode table I/O ---------------------------------------------------

    fn read_inode(&self, ino: u32) -> Result<Inode, FsError> {
        if ino >= self.sb.inode_count {
            return Err(FsError::NotFound);
        }
        let inodes_per_block = (BLOCK_SIZE / INODE_SIZE) as u64;
        let blk = self.sb.inode_start + ino as u64 / inodes_per_block;
        let off = (ino as u64 % inodes_per_block) as usize * INODE_SIZE;
        let mut buf = vec![0u8; BLOCK_SIZE];
        self.dev.read_blocks(blk, 1, &mut buf)?;
        Inode::read_from(&buf[off..off + INODE_SIZE])
    }

    fn write_inode(&mut self, ino: u32, inode: &Inode) -> Result<(), FsError> {
        if ino >= self.sb.inode_count {
            return Err(FsError::NotFound);
        }
        let inodes_per_block = (BLOCK_SIZE / INODE_SIZE) as u64;
        let blk = self.sb.inode_start + ino as u64 / inodes_per_block;
        let off = (ino as u64 % inodes_per_block) as usize * INODE_SIZE;
        let mut buf = vec![0u8; BLOCK_SIZE];
        self.dev.read_blocks(blk, 1, &mut buf)?;
        inode.write_to(&mut buf[off..off + INODE_SIZE])?;
        self.dev.write_blocks(blk, 1, &buf)?;
        Ok(())
    }

    fn alloc_inode(&mut self) -> Result<u32, FsError> {
        // Scan the inode table for a free slot (inode 0 is the root, never free).
        for ino in 1..self.sb.inode_count {
            if self.read_inode(ino)?.kind == KIND_FREE {
                return Ok(ino);
            }
        }
        Err(FsError::NoSpace)
    }

    // ---- block bitmap ------------------------------------------------------

    fn bit_get(&self, data_index: u64) -> Result<bool, FsError> {
        let byte = data_index / 8;
        let bit = (data_index % 8) as u8;
        let blk = self.sb.bitmap_start + byte / BLOCK_SIZE as u64;
        let off = (byte % BLOCK_SIZE as u64) as usize;
        let mut buf = vec![0u8; BLOCK_SIZE];
        self.dev.read_blocks(blk, 1, &mut buf)?;
        Ok(buf[off] & (1 << bit) != 0)
    }

    fn bit_set(&mut self, data_index: u64, value: bool) -> Result<(), FsError> {
        let byte = data_index / 8;
        let bit = (data_index % 8) as u8;
        let blk = self.sb.bitmap_start + byte / BLOCK_SIZE as u64;
        let off = (byte % BLOCK_SIZE as u64) as usize;
        let mut buf = vec![0u8; BLOCK_SIZE];
        self.dev.read_blocks(blk, 1, &mut buf)?;
        if value {
            buf[off] |= 1 << bit;
        } else {
            buf[off] &= !(1 << bit);
        }
        self.dev.write_blocks(blk, 1, &buf)?;
        Ok(())
    }

    /// Allocate a data block, returning its absolute LBA.
    fn alloc_block(&mut self) -> Result<u64, FsError> {
        for i in 0..self.sb.data_blocks {
            if !self.bit_get(i)? {
                self.bit_set(i, true)?;
                return Ok(self.sb.data_start + i);
            }
        }
        Err(FsError::NoSpace)
    }

    fn free_block(&mut self, lba: u64) -> Result<(), FsError> {
        if lba < self.sb.data_start {
            return Err(FsError::Corrupt);
        }
        self.bit_set(lba - self.sb.data_start, false)
    }

    // ---- file data ---------------------------------------------------------

    /// Read the entire contents of file/dir `inode` into a vector.
    fn read_inode_data(&self, inode: &Inode) -> Result<Vec<u8>, FsError> {
        let mut out = Vec::with_capacity(inode.size as usize);
        let mut remaining = inode.size as usize;
        for &blk in inode.direct.iter().take(inode.blocks_used()) {
            let mut buf = vec![0u8; BLOCK_SIZE];
            self.dev.read_blocks(blk, 1, &mut buf)?;
            let take = remaining.min(BLOCK_SIZE);
            out.extend_from_slice(&buf[..take]);
            remaining -= take;
        }
        Ok(out)
    }

    /// Replace the contents of inode `ino` with `data`, allocating/freeing data
    /// blocks as needed.
    fn write_inode_data(&mut self, ino: u32, inode: &mut Inode, data: &[u8]) -> Result<(), FsError> {
        if data.len() as u64 > MAX_FILE_SIZE {
            return Err(FsError::TooLarge);
        }
        let new_blocks = data.len().div_ceil(BLOCK_SIZE);
        let old_blocks = inode.blocks_used();

        // Allocate any additional blocks.
        for i in old_blocks..new_blocks {
            inode.direct[i] = self.alloc_block()?;
        }
        // Free any blocks no longer needed.
        for i in new_blocks..old_blocks {
            let lba = inode.direct[i];
            if lba != 0 {
                self.free_block(lba)?;
                inode.direct[i] = 0;
            }
        }

        // Write the data, zero-padding the final block.
        for i in 0..new_blocks {
            let start = i * BLOCK_SIZE;
            let end = (start + BLOCK_SIZE).min(data.len());
            let mut buf = vec![0u8; BLOCK_SIZE];
            buf[..end - start].copy_from_slice(&data[start..end]);
            self.dev.write_blocks(inode.direct[i], 1, &buf)?;
        }

        inode.size = data.len() as u64;
        self.write_inode(ino, inode)?;
        Ok(())
    }

    // ---- directory operations ----------------------------------------------

    /// Read a directory's entries (inode `dir_ino` must be a directory).
    fn read_dir_entries(&self, dir_ino: u32) -> Result<Vec<(u32, bool, Vec<u8>)>, FsError> {
        let dir = self.read_inode(dir_ino)?;
        if dir.kind != KIND_DIR {
            return Err(FsError::WrongKind);
        }
        let data = self.read_inode_data(&dir)?;
        let mut out = Vec::new();
        let mut off = 0;
        while off + DIR_ENTRY_SIZE <= data.len() {
            let ent = &data[off..off + DIR_ENTRY_SIZE];
            let mut r = ByteReader::new(ent);
            let ino = r.get_u32()?;
            let name_len = r.get_u8()? as usize;
            let name_field = r.get_slice(NAME_MAX)?;
            if ino != 0 && name_len <= NAME_MAX {
                let child = self.read_inode(ino)?;
                out.push((ino, child.kind == KIND_DIR, name_field[..name_len].to_vec()));
            }
            off += DIR_ENTRY_SIZE;
        }
        Ok(out)
    }

    fn encode_dir(entries: &[(u32, Vec<u8>)]) -> Result<Vec<u8>, FsError> {
        let mut data = vec![0u8; entries.len() * DIR_ENTRY_SIZE];
        for (i, (ino, name)) in entries.iter().enumerate() {
            let slot = &mut data[i * DIR_ENTRY_SIZE..(i + 1) * DIR_ENTRY_SIZE];
            let mut w = ByteWriter::new(slot);
            w.put_u32(*ino)?;
            w.put_u8(name.len() as u8)?;
            let mut namebuf = [0u8; NAME_MAX];
            namebuf[..name.len()].copy_from_slice(name);
            w.put_bytes(&namebuf)?;
            w.put_u16(0)?; // pad to DIR_ENTRY_SIZE
            w.put_u8(0)?;
        }
        Ok(data)
    }

    /// Add `(ino, name)` to directory `dir_ino`. Caller ensures `name` is unique.
    fn dir_add(&mut self, dir_ino: u32, ino: u32, name: &[u8]) -> Result<(), FsError> {
        if name.len() > NAME_MAX {
            return Err(FsError::TooLarge);
        }
        let existing = self.read_dir_entries(dir_ino)?;
        let mut entries: Vec<(u32, Vec<u8>)> =
            existing.into_iter().map(|(i, _, n)| (i, n)).collect();
        entries.push((ino, name.to_vec()));
        let data = Self::encode_dir(&entries)?;
        let mut dir = self.read_inode(dir_ino)?;
        self.write_inode_data(dir_ino, &mut dir, &data)
    }

    fn dir_lookup(&self, dir_ino: u32, name: &[u8]) -> Result<u32, FsError> {
        for (ino, _, n) in self.read_dir_entries(dir_ino)? {
            if n == name {
                return Ok(ino);
            }
        }
        Err(FsError::NotFound)
    }

    /// Remove the entry `name` from directory `dir_ino` by re-encoding the
    /// directory without it. Mirrors [`Self::dir_add`]. Returns
    /// [`FsError::NotFound`] if the entry is absent.
    fn dir_remove(&mut self, dir_ino: u32, name: &[u8]) -> Result<(), FsError> {
        let existing = self.read_dir_entries(dir_ino)?;
        let before = existing.len();
        let entries: Vec<(u32, Vec<u8>)> = existing
            .into_iter()
            .filter(|(_, _, n)| n != name)
            .map(|(i, _, n)| (i, n))
            .collect();
        if entries.len() == before {
            return Err(FsError::NotFound);
        }
        let data = Self::encode_dir(&entries)?;
        let mut dir = self.read_inode(dir_ino)?;
        self.write_inode_data(dir_ino, &mut dir, &data)
    }

    // ---- public path-based API ---------------------------------------------

    /// Split a `/`-separated path into components, ignoring empty parts.
    fn components(path: &[u8]) -> Vec<&[u8]> {
        path.split(|&c| c == b'/').filter(|s| !s.is_empty()).collect()
    }

    /// Resolve a path to its inode number.
    fn resolve(&self, path: &[u8]) -> Result<u32, FsError> {
        let mut ino = ROOT_INODE;
        for comp in Self::components(path) {
            ino = self.dir_lookup(ino, comp)?;
        }
        Ok(ino)
    }

    /// Resolve the parent directory of `path` and return `(parent_ino, name)`.
    fn resolve_parent<'p>(&self, path: &'p [u8]) -> Result<(u32, &'p [u8]), FsError> {
        let comps = Self::components(path);
        let (last, parents) = comps.split_last().ok_or(FsError::NotFound)?;
        let mut ino = ROOT_INODE;
        for comp in parents {
            ino = self.dir_lookup(ino, comp)?;
        }
        Ok((ino, last))
    }

    /// Create a directory at `path` (parent must exist).
    ///
    /// # Errors
    ///
    /// [`FsError::Exists`] if the name is taken, [`FsError::NotFound`] if the
    /// parent does not exist.
    pub fn mkdir(&mut self, path: &[u8]) -> Result<u32, FsError> {
        let (parent, name) = self.resolve_parent(path)?;
        if self.dir_lookup(parent, name).is_ok() {
            return Err(FsError::Exists);
        }
        let ino = self.alloc_inode()?;
        let node = Inode {
            kind: KIND_DIR,
            size: 0,
            direct: [0; DIRECT_BLOCKS],
        };
        self.write_inode(ino, &node)?;
        self.dir_add(parent, ino, name)?;
        Ok(ino)
    }

    /// Create (or truncate) a file at `path` and write `data` to it.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] if the parent does not exist, [`FsError::TooLarge`]
    /// if `data` exceeds [`MAX_FILE_SIZE`].
    pub fn write_file(&mut self, path: &[u8], data: &[u8]) -> Result<u32, FsError> {
        let (parent, name) = self.resolve_parent(path)?;
        let ino = match self.dir_lookup(parent, name) {
            Ok(existing) => existing,
            Err(FsError::NotFound) => {
                let ino = self.alloc_inode()?;
                let node = Inode {
                    kind: KIND_FILE,
                    size: 0,
                    direct: [0; DIRECT_BLOCKS],
                };
                self.write_inode(ino, &node)?;
                self.dir_add(parent, ino, name)?;
                ino
            }
            Err(e) => return Err(e),
        };
        let mut node = self.read_inode(ino)?;
        if node.kind != KIND_FILE {
            return Err(FsError::WrongKind);
        }
        self.write_inode_data(ino, &mut node, data)?;
        Ok(ino)
    }

    /// Read the entire contents of the file at `path`.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] / [`FsError::WrongKind`].
    pub fn read_file(&self, path: &[u8]) -> Result<Vec<u8>, FsError> {
        let ino = self.resolve(path)?;
        let node = self.read_inode(ino)?;
        if node.kind != KIND_FILE {
            return Err(FsError::WrongKind);
        }
        self.read_inode_data(&node)
    }

    /// List the entries of the directory at `path`.
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] / [`FsError::WrongKind`].
    pub fn list_dir(&self, path: &[u8]) -> Result<Vec<DirEntry>, FsError> {
        let ino = self.resolve(path)?;
        Ok(self
            .read_dir_entries(ino)?
            .into_iter()
            .map(|(inode, is_dir, name)| DirEntry { inode, is_dir, name })
            .collect())
    }

    /// Whether `path` exists.
    #[must_use]
    pub fn exists(&self, path: &[u8]) -> bool {
        self.resolve(path).is_ok()
    }

    /// Remove the file at `path`: free its data blocks, free its inode, and
    /// remove its directory entry. Only regular files are removed here;
    /// attempting to remove a directory returns [`FsError::WrongKind`].
    ///
    /// # Errors
    ///
    /// [`FsError::NotFound`] if the path does not exist; [`FsError::WrongKind`]
    /// if it names a directory.
    pub fn remove_file(&mut self, path: &[u8]) -> Result<(), FsError> {
        let (parent, name) = self.resolve_parent(path)?;
        let ino = self.dir_lookup(parent, name)?;
        let mut node = self.read_inode(ino)?;
        if node.kind != KIND_FILE {
            return Err(FsError::WrongKind);
        }
        // Free every data block the file holds.
        for &blk in node.direct.iter().take(node.blocks_used()) {
            if blk != 0 {
                self.free_block(blk)?;
            }
        }
        // Free the inode (mark it KIND_FREE with no blocks).
        node.kind = KIND_FREE;
        node.size = 0;
        node.direct = [0; DIRECT_BLOCKS];
        self.write_inode(ino, &node)?;
        // Unlink it from its parent directory.
        self.dir_remove(parent, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::check_range;
    use core::cell::RefCell;

    struct RamDisk {
        blocks: RefCell<Vec<u8>>,
        count: u64,
    }
    impl RamDisk {
        fn new(count: u64) -> Self {
            RamDisk {
                blocks: RefCell::new(vec![0u8; count as usize * BLOCK_SIZE]),
                count,
            }
        }
    }
    impl BlockDevice for RamDisk {
        fn block_count(&self) -> u64 {
            self.count
        }
        fn read_blocks(&self, lba: u64, count: u32, buf: &mut [u8]) -> Result<(), BlockError> {
            check_range(self.count, lba, count, buf.len())?;
            let s = lba as usize * BLOCK_SIZE;
            buf.copy_from_slice(&self.blocks.borrow()[s..s + count as usize * BLOCK_SIZE]);
            Ok(())
        }
        fn write_blocks(&self, lba: u64, count: u32, buf: &[u8]) -> Result<(), BlockError> {
            check_range(self.count, lba, count, buf.len())?;
            let s = lba as usize * BLOCK_SIZE;
            self.blocks.borrow_mut()[s..s + count as usize * BLOCK_SIZE].copy_from_slice(buf);
            Ok(())
        }
    }

    fn fresh(blocks: u64, inodes: u32) -> Fs<RamDisk> {
        Fs::format(RamDisk::new(blocks), inodes).unwrap()
    }

    #[test]
    fn format_then_mount_roundtrips_superblock() {
        let fs = fresh(256, 64);
        let sb = *fs.superblock();
        let dev = fs.into_device();
        let fs2 = Fs::mount(dev).unwrap();
        assert_eq!(*fs2.superblock(), sb);
        assert_eq!(sb.total_blocks, 256);
        assert_eq!(sb.inode_count, 64);
    }

    #[test]
    fn mount_rejects_unformatted() {
        let dev = RamDisk::new(64);
        assert_eq!(Fs::mount(dev).err(), Some(FsError::BadSuperblock));
    }

    #[test]
    fn write_read_file_in_root() {
        let mut fs = fresh(256, 64);
        fs.write_file(b"/hello.txt", b"persistent bytes").unwrap();
        assert_eq!(fs.read_file(b"/hello.txt").unwrap(), b"persistent bytes");
        assert!(fs.exists(b"/hello.txt"));
        assert!(!fs.exists(b"/nope"));
    }

    #[test]
    fn overwrite_grows_and_shrinks() {
        let mut fs = fresh(256, 64);
        let big = vec![7u8; BLOCK_SIZE + 100];
        fs.write_file(b"/f", &big).unwrap();
        assert_eq!(fs.read_file(b"/f").unwrap(), big);
        fs.write_file(b"/f", b"tiny").unwrap();
        assert_eq!(fs.read_file(b"/f").unwrap(), b"tiny");
    }

    #[test]
    fn directories_and_paths() {
        let mut fs = fresh(256, 64);
        fs.mkdir(b"/etc").unwrap();
        fs.write_file(b"/etc/passwd", b"user:hash").unwrap();
        assert_eq!(fs.read_file(b"/etc/passwd").unwrap(), b"user:hash");
        let listing = fs.list_dir(b"/etc").unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, b"passwd");
        assert!(!listing[0].is_dir);
        let root = fs.list_dir(b"/").unwrap();
        assert_eq!(root.len(), 1);
        assert_eq!(root[0].name, b"etc");
        assert!(root[0].is_dir);
    }

    #[test]
    fn nested_directories() {
        let mut fs = fresh(512, 128);
        fs.mkdir(b"/a").unwrap();
        fs.mkdir(b"/a/b").unwrap();
        fs.write_file(b"/a/b/c.txt", b"deep").unwrap();
        assert_eq!(fs.read_file(b"/a/b/c.txt").unwrap(), b"deep");
    }

    #[test]
    fn duplicate_mkdir_rejected() {
        let mut fs = fresh(256, 64);
        fs.mkdir(b"/x").unwrap();
        assert_eq!(fs.mkdir(b"/x").err(), Some(FsError::Exists));
    }

    #[test]
    fn persists_across_remount() {
        let mut fs = fresh(256, 64);
        fs.mkdir(b"/home").unwrap();
        fs.write_file(b"/home/note", b"survives reboot").unwrap();
        let dev = fs.into_device();
        let fs2 = Fs::mount(dev).unwrap();
        assert_eq!(fs2.read_file(b"/home/note").unwrap(), b"survives reboot");
        let home = fs2.list_dir(b"/home").unwrap();
        assert_eq!(home[0].name, b"note");
    }

    #[test]
    fn rejects_oversize_file() {
        let mut fs = fresh(256, 64);
        let too_big = vec![0u8; MAX_FILE_SIZE as usize + 1];
        assert_eq!(fs.write_file(b"/big", &too_big).err(), Some(FsError::TooLarge));
        let max = vec![1u8; MAX_FILE_SIZE as usize];
        fs.write_file(b"/max", &max).unwrap();
        assert_eq!(fs.read_file(b"/max").unwrap().len(), MAX_FILE_SIZE as usize);
    }

    #[test]
    fn block_reuse_after_shrink() {
        let mut fs = fresh(64, 32);
        fs.write_file(b"/a", &vec![1u8; 3 * BLOCK_SIZE]).unwrap();
        fs.write_file(b"/a", &vec![2u8; BLOCK_SIZE]).unwrap(); // shrink, frees 2
        fs.write_file(b"/b", &vec![3u8; 2 * BLOCK_SIZE]).unwrap(); // reuses freed
        assert_eq!(fs.read_file(b"/a").unwrap(), vec![2u8; BLOCK_SIZE]);
        assert_eq!(fs.read_file(b"/b").unwrap(), vec![3u8; 2 * BLOCK_SIZE]);
    }

    #[test]
    fn remove_file_unlinks_and_reclaims() {
        let mut fs = fresh(64, 32);
        fs.mkdir(b"/etc").unwrap();
        fs.write_file(b"/etc/a", &vec![1u8; 2 * BLOCK_SIZE]).unwrap();
        fs.write_file(b"/etc/b", b"keep").unwrap();
        assert_eq!(fs.list_dir(b"/etc").unwrap().len(), 2);

        // Remove /etc/a: it disappears from the listing, the other stays.
        fs.remove_file(b"/etc/a").unwrap();
        let listing = fs.list_dir(b"/etc").unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, b"b");
        assert!(!fs.exists(b"/etc/a"));
        assert_eq!(fs.read_file(b"/etc/b").unwrap(), b"keep");

        // Removing a missing file -> NotFound.
        assert_eq!(fs.remove_file(b"/etc/a").err(), Some(FsError::NotFound));
        // Removing a directory -> WrongKind.
        assert_eq!(fs.remove_file(b"/etc").err(), Some(FsError::WrongKind));

        // The freed blocks/inode are reusable: write a new file that needs them.
        fs.write_file(b"/etc/c", &vec![7u8; 2 * BLOCK_SIZE]).unwrap();
        assert_eq!(fs.read_file(b"/etc/c").unwrap(), vec![7u8; 2 * BLOCK_SIZE]);
    }

    #[test]
    fn remove_persists_across_remount() {
        let mut fs = fresh(128, 64);
        fs.write_file(b"/x", b"gone soon").unwrap();
        fs.write_file(b"/y", b"stays").unwrap();
        fs.remove_file(b"/x").unwrap();
        let dev = fs.into_device();
        let fs2 = Fs::mount(dev).unwrap();
        assert!(!fs2.exists(b"/x"));
        assert_eq!(fs2.read_file(b"/y").unwrap(), b"stays");
    }
}
