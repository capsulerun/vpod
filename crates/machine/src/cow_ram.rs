// Copy-on-write guest RAM.
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

pub const PAGE_SIZE: usize = 4096;

static EPOCH_SOURCE: AtomicU64 = AtomicU64::new(1);

fn next_epoch() -> u64 {
    EPOCH_SOURCE.fetch_add(1, Ordering::Relaxed)
}

pub struct CowRam {
    base: Arc<Vec<u8>>,
    pages: Vec<Option<Box<[u8]>>>,
    len: usize,
    mask: u64,
    epoch: u64,
}

impl CowRam {
    pub fn new(ram_size: u64) -> Self {
        let logical_len = ram_size as usize + 8;
        Self::from_padded(
            vec![0u8; Self::padded_len(logical_len)],
            logical_len,
            ram_size,
        )
    }

    pub fn from_base(bytes: Vec<u8>, ram_size: u64) -> Self {
        let len = bytes.len();
        let num_pages = len.div_ceil(PAGE_SIZE);

        let mut base = bytes;

        base.reserve_exact((num_pages * PAGE_SIZE).saturating_sub(len));
        base.resize(num_pages * PAGE_SIZE, 0);

        Self {
            base: Arc::new(base),
            pages: vec![None; num_pages],
            len,
            mask: ram_size - 1,
            epoch: next_epoch(),
        }
    }

    pub fn padded_len(logical_len: usize) -> usize {
        logical_len.div_ceil(PAGE_SIZE) * PAGE_SIZE
    }

    pub fn from_padded(padded: Vec<u8>, logical_len: usize, ram_size: u64) -> Self {
        debug_assert_eq!(padded.len(), Self::padded_len(logical_len));
        let num_pages = padded.len() / PAGE_SIZE;

        Self {
            base: Arc::new(padded),
            pages: vec![None; num_pages],
            len: logical_len,
            mask: ram_size - 1,
            epoch: next_epoch(),
        }
    }

    pub fn clone_shared(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            pages: vec![None; self.pages.len()],
            len: self.len,
            mask: self.mask,
            epoch: next_epoch(),
        }
    }

    #[inline(always)]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    #[inline(always)]
    pub fn page_ptr(&self, page: usize) -> *const u8 {
        self.page_ref(page).as_ptr()
    }

    #[inline(always)]
    pub fn page_mut_ptr(&mut self, page: usize) -> *mut u8 {
        self.page_mut(page).as_mut_ptr()
    }

    #[inline(always)]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline(always)]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline(always)]
    pub fn mask(&self) -> u64 {
        self.mask
    }

    #[inline(always)]
    fn page_ref(&self, page: usize) -> &[u8] {
        match &self.pages[page] {
            Some(p) => p,
            None => &self.base[page * PAGE_SIZE..(page + 1) * PAGE_SIZE],
        }
    }

    #[inline(always)]
    fn page_mut(&mut self, page: usize) -> &mut [u8] {
        if self.pages[page].is_none() {
            let start = page * PAGE_SIZE;
            let mut owned = vec![0u8; PAGE_SIZE].into_boxed_slice();
            owned.copy_from_slice(&self.base[start..start + PAGE_SIZE]);
            self.pages[page] = Some(owned);

            self.epoch = next_epoch();
        }

        self.pages[page].as_mut().unwrap()
    }

    #[inline(always)]
    pub fn read_u8(&self, idx: usize) -> u8 {
        self.page_ref(idx >> 12)[idx & (PAGE_SIZE - 1)]
    }

    #[inline(always)]
    pub fn write_u8(&mut self, idx: usize, val: u8) {
        self.page_mut(idx >> 12)[idx & (PAGE_SIZE - 1)] = val;
    }

    #[inline(always)]
    pub fn read_u16(&self, idx: usize) -> u16 {
        let off = idx & (PAGE_SIZE - 1);

        if off + 2 <= PAGE_SIZE {
            let p = self.page_ref(idx >> 12);

            u16::from_le_bytes([p[off], p[off + 1]])
        } else {
            u16::from_le_bytes([self.read_u8(idx), self.read_u8(idx + 1)])
        }
    }

    #[inline(always)]
    pub fn read_u32(&self, idx: usize) -> u32 {
        let off = idx & (PAGE_SIZE - 1);

        if off + 4 <= PAGE_SIZE {
            let p = self.page_ref(idx >> 12);

            u32::from_le_bytes(p[off..off + 4].try_into().unwrap())
        } else {
            let mut b = [0u8; 4];

            self.read_into(idx, &mut b);
            u32::from_le_bytes(b)
        }
    }

    #[inline(always)]
    pub fn read_u64(&self, idx: usize) -> u64 {
        let off = idx & (PAGE_SIZE - 1);

        if off + 8 <= PAGE_SIZE {
            let p = self.page_ref(idx >> 12);

            u64::from_le_bytes(p[off..off + 8].try_into().unwrap())
        } else {
            let mut b = [0u8; 8];

            self.read_into(idx, &mut b);
            u64::from_le_bytes(b)
        }
    }

    #[inline(always)]
    pub fn write_u16(&mut self, idx: usize, val: u16) {
        let off = idx & (PAGE_SIZE - 1);

        if off + 2 <= PAGE_SIZE {
            self.page_mut(idx >> 12)[off..off + 2].copy_from_slice(&val.to_le_bytes());
        } else {
            self.write_from(idx, &val.to_le_bytes());
        }
    }

    #[inline(always)]
    pub fn write_u32(&mut self, idx: usize, val: u32) {
        let off = idx & (PAGE_SIZE - 1);

        if off + 4 <= PAGE_SIZE {
            self.page_mut(idx >> 12)[off..off + 4].copy_from_slice(&val.to_le_bytes());
        } else {
            self.write_from(idx, &val.to_le_bytes());
        }
    }

    #[inline(always)]
    pub fn write_u64(&mut self, idx: usize, val: u64) {
        let off = idx & (PAGE_SIZE - 1);

        if off + 8 <= PAGE_SIZE {
            self.page_mut(idx >> 12)[off..off + 8].copy_from_slice(&val.to_le_bytes());
        } else {
            self.write_from(idx, &val.to_le_bytes());
        }
    }

    pub fn read_into(&self, mut idx: usize, buf: &mut [u8]) {
        let mut written = 0;

        while written < buf.len() {
            let page = idx >> 12;
            let off = idx & (PAGE_SIZE - 1);
            let n = (PAGE_SIZE - off).min(buf.len() - written);

            buf[written..written + n].copy_from_slice(&self.page_ref(page)[off..off + n]);
            written += n;
            idx += n;
        }
    }

    pub fn write_from(&mut self, mut idx: usize, buf: &[u8]) {
        let mut read = 0;

        while read < buf.len() {
            let page = idx >> 12;
            let off = idx & (PAGE_SIZE - 1);
            let n = (PAGE_SIZE - off).min(buf.len() - read);

            self.page_mut(page)[off..off + n].copy_from_slice(&buf[read..read + n]);
            read += n;
            idx += n;
        }
    }

    pub fn dirty_pages(&self) -> Vec<u32> {
        self.pages
            .iter()
            .enumerate()
            .filter_map(|(i, p)| p.as_ref().map(|_| i as u32))
            .collect()
    }

    pub fn page_bytes(&self, page: usize) -> &[u8] {
        self.page_ref(page)
    }

    pub fn apply_page(&mut self, page: usize, bytes: &[u8; PAGE_SIZE]) {
        self.page_mut(page).copy_from_slice(bytes);
    }

    pub fn num_pages(&self) -> usize {
        self.pages.len()
    }

    pub fn write_all_to(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        let mut remaining = self.len;
        let mut page = 0;

        while remaining > 0 {
            let n = remaining.min(PAGE_SIZE);
            writer.write_all(&self.page_ref(page)[..n])?;
            remaining -= n;
            page += 1;
        }
        Ok(())
    }

    pub fn set_base(&mut self, padded: Vec<u8>) {
        debug_assert_eq!(padded.len(), Self::padded_len(self.len));
        let num_pages = padded.len() / PAGE_SIZE;

        self.base = Arc::new(padded);
        self.pages = vec![None; num_pages];
        self.epoch = next_epoch();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WASM32_ISIZE_MAX: usize = (1usize << 31) - 1;

    #[test]
    fn new_allocates_page_padded_and_never_grows() {
        for mb in [1u64, 64, 256, 512, 1024] {
            let ram = mb * 1024 * 1024;
            let cow = CowRam::new(ram);

            let allocated = cow.base.len();
            assert_eq!(
                allocated % PAGE_SIZE,
                0,
                "{mb}MB: base is not page-aligned, so resize would grow it"
            );
            assert_eq!(
                allocated,
                CowRam::padded_len(ram as usize + 8),
                "{mb}MB: base is not the size the old resize would have produced"
            );
            assert_eq!(cow.len, ram as usize + 8, "{mb}MB: logical len changed");
            assert_eq!(cow.pages.len(), allocated / PAGE_SIZE, "{mb}MB: page count");

            // What the removed `resize` would have asked for: `max(cap * 2,
            // required)`. This is the assertion that would have caught the bug.
            let first_alloc = ram as usize + 8;
            let would_have_requested = (first_alloc * 2).max(allocated);
            if mb == 1024 {
                assert!(
                    would_have_requested > WASM32_ISIZE_MAX,
                    "the 1024MB case is supposed to be the one that overflowed"
                );
            }
            assert!(
                allocated <= WASM32_ISIZE_MAX,
                "{mb}MB: a single allocation of {allocated} cannot be made on wasm32"
            );
        }
    }

    #[test]
    fn two_gigabytes_is_out_of_reach_on_wasm32() {
        let padded = CowRam::padded_len(2048 * 1024 * 1024usize + 8);
        assert!(
            padded > WASM32_ISIZE_MAX,
            "2048MB would fit on wasm32; this test and the ceiling it documents are stale"
        );

        let ceiling = CowRam::padded_len(1024 * 1024 * 1024usize + 8);
        assert!(ceiling <= WASM32_ISIZE_MAX, "1024MB must remain reachable");
    }

    #[test]
    fn new_is_zeroed_and_addressable() {
        let cow = CowRam::new(4 * 1024 * 1024);
        assert_eq!(cow.read_u8(0), 0);
        assert_eq!(cow.read_u8(4 * 1024 * 1024 - 1), 0);
    }
}
