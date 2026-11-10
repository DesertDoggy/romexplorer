//! seeddb.bin: seeds for titles using "seed crypto" (introduced in 9.6.0-24).
//!
//! Layout: u32 LE count, 12 bytes padding, then `count` entries of
//! { u64 LE title id, 16-byte seed, 8 bytes padding }.

use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::Path;

#[derive(Default, Clone)]
pub struct SeedDb {
    seeds: HashMap<u64, [u8; 16]>,
}

impl SeedDb {
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 0x10 {
            bail!("seeddb is too small");
        }
        let count = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let mut seeds = HashMap::new();
        for i in 0..count {
            let off = 0x10 + i * 0x20;
            let Some(e) = data.get(off..off + 0x20) else {
                break;
            };
            let tid = u64::from_le_bytes(e[0..8].try_into().unwrap());
            seeds.insert(tid, e[8..0x18].try_into().unwrap());
        }
        Ok(SeedDb { seeds })
    }

    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path)
            .with_context(|| format!("cannot read seeddb {}", path.display()))?;
        Self::parse(&data)
    }

    pub fn get(&self, title_id: u64) -> Option<[u8; 16]> {
        self.seeds.get(&title_id).copied()
    }

    pub fn insert(&mut self, title_id: u64, seed: [u8; 16]) {
        self.seeds.insert(title_id, seed);
    }

    pub fn len(&self) -> usize {
        self.seeds.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seeds.is_empty()
    }
}
