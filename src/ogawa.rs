//! Ogawa: the container Alembic files are stored in (see
//! [`crate::alembic`] for what's inside). A tree of groups and data:
//!
//! - The file starts `Ogawa`, a byte that is 0xff once the file was
//!   closed properly, the format version (0, 1) and the offset of the root
//!   group, a little-endian u64.
//! - A group is a u64 count, then each child's u64 offset: with the top
//!   bit set it's data, else a group; offset 0 is an empty one.
//! - Data is a u64 size, then that many bytes.
//!
//! The file is read where asked (caches run to gigabytes), every offset and
//! size checked against its length, so a broken file errors rather than
//! panics.

use anyhow::{bail, Context, Result};
use std::path::Path;

const DATA_BIT: u64 = 1 << 63;

/// A child of a group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Child {
    Group(u64),
    Data(u64),
}

impl Child {
    fn of(raw: u64) -> Child {
        if raw & DATA_BIT != 0 { Child::Data(raw & !DATA_BIT) } else { Child::Group(raw) }
    }
}

/// An open Ogawa file.
pub struct Ogawa {
    file: std::fs::File,
    len: u64,
    root: u64,
}

impl Ogawa {
    pub fn open(path: &Path) -> Result<Ogawa> {
        let file = std::fs::File::open(path).with_context(|| format!("can't read {}", path.display()))?;
        let len = file.metadata()?.len();
        let mut o = Ogawa { file, len, root: 0 };
        let head = o.bytes(0, 16).context("not an Ogawa file (too short)")?;
        if &head[..5] != b"Ogawa" {
            bail!("not an Ogawa file");
        }
        if head[5] != 0xff {
            bail!("the file wasn't finished being written");
        }
        if head[6..8] != [0, 1] {
            bail!("Ogawa version {}.{} isn't supported", head[6], head[7]);
        }
        o.root = u64::from_le_bytes(head[8..16].try_into().unwrap());
        Ok(o)
    }

    /// The root group's children.
    pub fn root(&self) -> Result<Vec<Child>> {
        self.group(self.root)
    }

    /// Group `at`'s children.
    pub fn group(&self, at: u64) -> Result<Vec<Child>> {
        if at == 0 {
            return Ok(Vec::new());
        }
        let n = self.u64(at)?;
        // Each child takes 8 bytes: a count past the file is broken.
        if n > (self.len - at.min(self.len)) / 8 {
            bail!("a group at {at} runs past the end of the file");
        }
        let raw = self.bytes(at + 8, n * 8)?;
        Ok(raw.as_chunks::<8>().0.iter().map(|c| Child::of(u64::from_le_bytes(*c))).collect())
    }

    /// Data `at`'s bytes.
    pub fn data(&self, at: u64) -> Result<Vec<u8>> {
        if at == 0 {
            return Ok(Vec::new());
        }
        let n = self.u64(at)?;
        self.bytes(at + 8, n)
    }

    /// A child that must be data: its bytes.
    pub fn data_of(&self, c: Child) -> Result<Vec<u8>> {
        match c {
            Child::Data(at) => self.data(at),
            Child::Group(_) => bail!("expected data, found a group"),
        }
    }

    /// A child that must be a group: its children.
    pub fn group_of(&self, c: Child) -> Result<Vec<Child>> {
        match c {
            Child::Group(at) => self.group(at),
            Child::Data(_) => bail!("expected a group, found data"),
        }
    }

    fn u64(&self, at: u64) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes(at, 8)?.try_into().unwrap()))
    }

    fn bytes(&self, at: u64, n: u64) -> Result<Vec<u8>> {
        if at.checked_add(n).is_none_or(|end| end > self.len) {
            bail!("the file ends early ({n} bytes at {at} of {})", self.len);
        }
        let mut buf = vec![0u8; n as usize];
        read_at(&self.file, &mut buf, at).with_context(|| format!("reading {n} bytes at {at}"))?;
        Ok(buf)
    }
}

#[cfg(unix)]
fn read_at(file: &std::fs::File, buf: &mut [u8], at: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buf, at)
}

#[cfg(windows)]
fn read_at(file: &std::fs::File, mut buf: &mut [u8], mut at: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buf, at)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                at += n as u64;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds Ogawa files for tests: groups and data appended, the root's
    /// offset written last.
    pub(crate) struct Writer(pub Vec<u8>);

    impl Writer {
        pub fn new() -> Writer {
            let mut b = b"Ogawa".to_vec();
            b.extend([0xff, 0, 1]);
            b.extend([0u8; 8]);
            Writer(b)
        }
        pub fn data(&mut self, bytes: &[u8]) -> u64 {
            let at = self.0.len() as u64;
            self.0.extend((bytes.len() as u64).to_le_bytes());
            self.0.extend(bytes);
            at | DATA_BIT
        }
        pub fn group(&mut self, children: &[u64]) -> u64 {
            let at = self.0.len() as u64;
            self.0.extend((children.len() as u64).to_le_bytes());
            for c in children {
                self.0.extend(c.to_le_bytes());
            }
            at
        }
        pub fn finish(mut self, root: u64) -> Vec<u8> {
            self.0[8..16].copy_from_slice(&root.to_le_bytes());
            self.0
        }
    }

    #[test]
    fn groups_and_data_read_back_and_broken_files_say_so() {
        let dir = std::env::temp_dir().join(format!("arclight_ogawa_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = Writer::new();
        let (a, b) = (w.data(b"hello"), w.data(b""));
        let inner = w.group(&[a, 0]);
        let root = w.group(&[inner, b, DATA_BIT]);
        let bytes = w.finish(root);
        let path = dir.join("t.ogawa");
        std::fs::write(&path, &bytes).unwrap();
        let o = Ogawa::open(&path).unwrap();
        let top = o.root().unwrap();
        assert_eq!(top.len(), 3);
        let kids = o.group_of(top[0]).unwrap();
        assert_eq!((o.data_of(kids[0]).unwrap(), kids[1]), (b"hello".to_vec(), Child::Group(0)));
        assert!(o.group_of(kids[1]).unwrap().is_empty() && o.data_of(top[1]).unwrap().is_empty() && o.data_of(top[2]).unwrap().is_empty());
        assert!(o.data_of(top[0]).is_err() && o.group_of(kids[0]).is_err(), "kinds are checked");
        // Broken: not Ogawa, unfinished, cut short, a count past the end.
        let open = |name: &str, b: &[u8]| {
            std::fs::write(dir.join(name), b).unwrap();
            Ogawa::open(&dir.join(name)).and_then(|o| o.root().map(|_| ())).map_err(|e| format!("{e:#}"))
        };
        assert!(open("x", b"HDF5 not really ogawa").unwrap_err().contains("not an Ogawa"));
        let mut unfinished = bytes.clone();
        unfinished[5] = 0;
        assert!(open("u", &unfinished).unwrap_err().contains("finished"));
        assert!(open("s", &bytes[..bytes.len() - 4]).unwrap_err().contains("ends early"));
        let mut huge = bytes.clone();
        let at = root as usize;
        huge[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(open("h", &huge).unwrap_err().contains("past the end"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
