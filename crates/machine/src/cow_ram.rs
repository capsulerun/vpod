// Copy-on-write guest RAM.
use std::sync::Arc;

pub const PAGE_SIZE: usize = 4096;

pub struct CowRam {
    base: Arc<Vec<u8>>,
    pages: Vec<Option<Box<[u8]>>>,
    len: usize,
    mask: u64,
}

impl CowRam {
    pub fn new(ram_size: u64) -> Self {
        Self::from_base(vec![0u8; ram_size as usize + 8], ram_size)
    }

    pub fn from_base(bytes: Vec<u8>, ram_size: u64) -> Self {
        let len = bytes.len();
        let num_pages = len.div_ceil(PAGE_SIZE);

        let mut base = bytes;
        base.resize(num_pages * PAGE_SIZE, 0);

        Self {
            base: Arc::new(base),
            pages: vec![None; num_pages],
            len,
            mask: ram_size - 1,
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
        }
    }

    pub fn clone_shared(&self) -> Self {
        Self {
            base: Arc::clone(&self.base),
            pages: vec![None; self.pages.len()],
            len: self.len,
            mask: self.mask,
        }
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
        let base = &self.base;

        self.pages[page].get_or_insert_with(|| {
            let mut owned = vec![0u8; PAGE_SIZE].into_boxed_slice();
            owned.copy_from_slice(&base[page * PAGE_SIZE..(page + 1) * PAGE_SIZE]);

            owned
        })
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
    }
}
