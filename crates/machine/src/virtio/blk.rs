use std::io::{Read, Seek, SeekFrom, Write};

use super::{RamView, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE, VirtioMmio};

const DEVICE_ID: u32 = 2;
const VIRTIO_F_VERSION_1: u64 = 1u64 << 32;
const DEVICE_FEATURES: u64 = (1 << 2) | (1 << 6) | VIRTIO_F_VERSION_1;

const SECTOR_SIZE: u64 = 512;

const BLK_T_IN: u32 = 0;
const BLK_T_OUT: u32 = 1;
const BLK_T_FLUSH: u32 = 4;

const BLK_S_OK: u8 = 0;
const BLK_S_IOERR: u8 = 1;
const BLK_S_UNSUPP: u8 = 2;

pub struct VirtioBlk {
    pub mmio: VirtioMmio,
    file: std::fs::File,
    sector_count: u64,
}

impl VirtioBlk {
    pub fn new(mut file: std::fs::File) -> std::io::Result<Self> {
        let size = file.seek(SeekFrom::End(0))?;
        let sector_count = size / SECTOR_SIZE;

        let mut dev = Self {
            mmio: VirtioMmio::new(DEVICE_ID, DEVICE_FEATURES, 1),
            file,
            sector_count,
        };

        let cap = sector_count.to_le_bytes();
        dev.mmio.config[0..8].copy_from_slice(&cap);

        let blk_size = SECTOR_SIZE as u32;
        dev.mmio.config[20..24].copy_from_slice(&blk_size.to_le_bytes());

        Ok(dev)
    }

    pub fn notify(&mut self, ram: &mut RamView) {
        while let Some(head) = self.mmio.queues[0].pop_avail(ram) {
            let used_len = self.process_request(ram, head);

            self.mmio.queues[0].push_used(ram, head, used_len);
            self.mmio.int_status |= 1;
        }
    }

    fn process_request(&mut self, ram: &mut RamView, head: u16) -> u32 {
        let mut read_bufs: Vec<(u64, u32)> = Vec::new();
        let mut write_bufs: Vec<(u64, u32)> = Vec::new();

        let mut desc = self.mmio.queues[0].read_desc(ram, head);

        loop {
            if desc.flags & VRING_DESC_F_WRITE != 0 {
                write_bufs.push((desc.addr, desc.len));
            } else {
                read_bufs.push((desc.addr, desc.len));
            }

            if desc.flags & VRING_DESC_F_NEXT == 0 {
                break;
            }

            desc = self.mmio.queues[0].read_desc(ram, desc.next);
        }

        let Some(&(hdr_addr, hdr_len)) = read_bufs.first() else {
            return 0;
        };

        if hdr_len < 16 {
            return 0;
        }

        let req_type = ram.read_u32(hdr_addr);
        let sector = ram.read_u64(hdr_addr + 8);

        let Some(&(status_addr, _)) = write_bufs.last() else {
            return 0;
        };

        let status = match req_type {
            BLK_T_IN => self.do_read(ram, sector, &write_bufs),
            BLK_T_OUT => self.do_write(ram, sector, &read_bufs[1..]),
            BLK_T_FLUSH => {
                let _ = self.file.flush();
                BLK_S_OK
            }
            _ => BLK_S_UNSUPP,
        };

        ram.write_u8(status_addr, status);

        let data_written: u32 = if req_type == BLK_T_IN {
            write_bufs
                .iter()
                .take(write_bufs.len().saturating_sub(1))
                .map(|(_, l)| l)
                .sum()
        } else {
            0
        };

        data_written + 1
    }

    fn do_read(&mut self, ram: &mut RamView, sector: u64, write_bufs: &[(u64, u32)]) -> u8 {
        let data_bufs = write_bufs.split_last().map(|(_, rest)| rest).unwrap_or(&[]);

        for &(addr, len) in data_bufs {
            if let Err(e) = self.read_sectors(ram, addr, len, sector) {
                log::warn!("virtio-blk read error at sector {sector}: {e}");
                return BLK_S_IOERR;
            }
        }

        BLK_S_OK
    }

    fn do_write(&mut self, ram: &mut RamView, sector: u64, read_bufs: &[(u64, u32)]) -> u8 {
        for &(addr, len) in read_bufs {
            if let Err(e) = self.write_sectors(ram, addr, len, sector) {
                log::warn!("virtio-blk write error at sector {sector}: {e}");
                return BLK_S_IOERR;
            }
        }

        BLK_S_OK
    }

    fn read_sectors(
        &mut self,
        ram: &mut RamView,
        dest: u64,
        len: u32,
        sector: u64,
    ) -> std::io::Result<()> {
        let end_sector = sector + len as u64 / SECTOR_SIZE;
        if end_sector > self.sector_count {
            return Err(std::io::Error::other("sector out of range"));
        }
        self.file.seek(SeekFrom::Start(sector * SECTOR_SIZE))?;

        let i = ((dest - crate::RAM_BASE) & ram.mask) as usize;
        self.file.read_exact(&mut ram.ram[i..i + len as usize])?;
        Ok(())
    }

    fn write_sectors(
        &mut self,
        ram: &mut RamView,
        src: u64,
        len: u32,
        sector: u64,
    ) -> std::io::Result<()> {
        let end_sector = sector + len as u64 / SECTOR_SIZE;
        if end_sector > self.sector_count {
            return Err(std::io::Error::other("sector out of range"));
        }

        self.file.seek(SeekFrom::Start(sector * SECTOR_SIZE))?;
        let i = ((src - crate::RAM_BASE) & ram.mask) as usize;
        self.file.write_all(&ram.ram[i..i + len as usize])?;
        Ok(())
    }
}
