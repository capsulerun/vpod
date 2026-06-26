// See FUSE protocol docs to have more information on ram writes/reads

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::io::Seek;

use super::{RamView, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE, VirtioMmio};

const DEVICE_ID: u32 = 26; // VIRTIO_DEVICE_ID_FS
const VIRTIO_F_VERSION_1: u64 = 1u64 << 32;
const DEVICE_FEATURES: u64 = VIRTIO_F_VERSION_1;

const FUSE_KERNEL_VERSION: u32 = 7;
const FUSE_KERNEL_MINOR_VERSION: u32 = 31;

const FUSE_LOOKUP: u32 = 1;
const FUSE_GETATTR: u32 = 3;
const FUSE_OPEN: u32 = 14;
const FUSE_READ: u32 = 15;
const FUSE_RELEASE: u32 = 18;
const FUSE_READDIR: u32 = 28;
const FUSE_INIT: u32 = 26;
const FUSE_STATFS: u32 = 17;
const FUSE_READDIRPLUS: u32 = 44;
const FUSE_OPENDIR: u32 = 27;
const FUSE_RELEASEDIR: u32 = 29;
const FUSE_FORGET: u32 = 2;
const FUSE_BATCH_FORGET: u32 = 42;

const ENOENT: i32 = -2;
const ENOTDIR: i32 = -20;
const ENOSYS: i32 = -38;
const EBADF: i32 = -9;

const FUSE_ROOT_ID: u64 = 1;

#[repr(C)]
struct FuseInHeader {
    len: u32,
    opcode: u32,
    unique: u64,
    nodeid: u64,
    uid: u32,
    gid: u32,
    pid: u32,
    _padding: u32,
}

#[derive(Clone)]
struct Inode {
    path: PathBuf,
}

struct FileHandle {
    path: PathBuf,
    is_dir: bool,
}

pub struct Mount {
    pub host_path: PathBuf,
    pub tag: String,
}

pub struct VirtioFs {
    pub mmio: VirtioMmio,
    inodes: HashMap<u64, Inode>,
    next_inode: u64,
    file_handles: HashMap<u64, FileHandle>,
    next_fh: u64,
    mounts: Vec<Mount>,
}

impl VirtioFs {
    pub fn new(mounts: Vec<Mount>) -> Self {
        let mut device = Self {
            mmio: VirtioMmio::new(DEVICE_ID, DEVICE_FEATURES, 2),
            inodes: HashMap::new(),
            next_inode: FUSE_ROOT_ID + 1,
            file_handles: HashMap::new(),
            next_fh: 1,
            mounts,
        };

        let tag = b"virtiofs";
        device.mmio.config[..tag.len()].copy_from_slice(tag);
        device.mmio.config[36..40].copy_from_slice(&1u32.to_le_bytes());

        device
    }

    fn root_path(&self) -> Option<&Path> {
        self.mounts.first().map(|m| m.host_path.as_path())
    }

    pub fn notify(&mut self, queue_index: usize, ram: &mut RamView) {
        if queue_index == 0 {
            return;
        }

        while let Some(head) = self.mmio.queues[queue_index].pop_avail(ram) {
            let used_len = self.process_request(ram, queue_index, head);
            self.mmio.queues[queue_index].push_used(ram, head, used_len);
            self.mmio.int_status |= 1;
        }
    }

    fn process_request(&mut self, ram: &mut RamView, queue_index: usize, head: u16) -> u32 {
        let mut read_bufs: Vec<(u64, u32)> = Vec::new();
        let mut write_bufs: Vec<(u64, u32)> = Vec::new();

        let mut desc = self.mmio.queues[queue_index].read_desc(ram, head);

        loop {
            if desc.flags & VRING_DESC_F_WRITE != 0 {
                write_bufs.push((desc.addr, desc.len));
            } else {
                read_bufs.push((desc.addr, desc.len));
            }
            if desc.flags & VRING_DESC_F_NEXT == 0 {
                break;
            }
            desc = self.mmio.queues[queue_index].read_desc(ram, desc.next);
        }

        if read_bufs.is_empty() || write_bufs.is_empty() {
            return 0;
        }

        let (header_addr, header_len) = read_bufs[0];
        if header_len < 40 {
            return 0;
        }

        let header = FuseInHeader {
            len: ram.read_u32(header_addr),
            opcode: ram.read_u32(header_addr + 4),
            unique: ram.read_u64(header_addr + 8),
            nodeid: ram.read_u64(header_addr + 16),
            uid: ram.read_u32(header_addr + 24),
            gid: ram.read_u32(header_addr + 28),
            pid: ram.read_u32(header_addr + 32),
            _padding: ram.read_u32(header_addr + 36),
        };

        let in_body_addr = header_addr + 40;
        let in_body_len = header_len.saturating_sub(40);

        let (out_addr, out_len) = write_bufs[0];

        match header.opcode {
            FUSE_INIT => self.init(&header, out_addr, out_len, ram),
            FUSE_LOOKUP => self.lookup(&header, ram, in_body_addr, in_body_len, out_addr, out_len),
            FUSE_GETATTR => self.getattr(&header, out_addr, out_len, ram),
            FUSE_OPEN | FUSE_OPENDIR => self.open(&header, ram, in_body_addr, out_addr, out_len),
            FUSE_READ => self.read(&header, ram, in_body_addr, out_addr, out_len),
            FUSE_READDIR => self.readdir(&header, ram, in_body_addr, out_addr, out_len, false),
            FUSE_READDIRPLUS => self.readdir(&header, ram, in_body_addr, out_addr, out_len, true),
            FUSE_RELEASE | FUSE_RELEASEDIR => {
                self.release(&header, ram, in_body_addr, out_addr, out_len)
            }
            FUSE_STATFS => self.statfs(&header, out_addr, out_len, ram),
            FUSE_FORGET | FUSE_BATCH_FORGET => 0,
            _ => self.reply_error(&header, ENOSYS, out_addr, ram),
        }
    }

    fn init(&self, header: &FuseInHeader, out_addr: u64, _out_len: u32, ram: &mut RamView) -> u32 {
        let out_header_size = 16u32;
        let init_out_size = 64u32;
        let total = out_header_size + init_out_size;

        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);

        let body = out_addr + 16;
        for i in 0..init_out_size {
            ram.write_u8(body + i as u64, 0);
        }

        ram.write_u32(body, FUSE_KERNEL_VERSION);
        ram.write_u32(body + 4, FUSE_KERNEL_MINOR_VERSION);
        ram.write_u32(body + 8, 128 * 1024);
        ram.write_u32(body + 12, 0);
        ram.write_u16(body + 16, 16);
        ram.write_u16(body + 18, 12);
        ram.write_u32(body + 20, 128 * 1024);

        total
    }

    fn lookup(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        in_body_len: u32,
        out_addr: u64,
        _out_len: u32,
    ) -> u32 {
        let name = self.read_cstring(ram, in_body_addr, in_body_len);

        let parent_path = if header.nodeid == FUSE_ROOT_ID {
            match self.root_path() {
                Some(p) => p.to_path_buf(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        } else {
            match self.inodes.get(&header.nodeid) {
                Some(inode) => inode.path.clone(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        };

        let child_path = parent_path.join(&name);

        let metadata = match fs::metadata(&child_path) {
            Ok(m) => m,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let ino = self.get_or_create_inode(&child_path, metadata.is_dir());

        self.write_entry_out(header, ram, out_addr, ino, &metadata)
    }

    fn getattr(
        &self,
        header: &FuseInHeader,
        out_addr: u64,
        _out_len: u32,
        ram: &mut RamView,
    ) -> u32 {
        let path = if header.nodeid == FUSE_ROOT_ID {
            match self.root_path() {
                Some(p) => p.to_path_buf(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        } else {
            match self.inodes.get(&header.nodeid) {
                Some(inode) => inode.path.clone(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        };

        let metadata = match fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        self.write_attr_out(header, ram, out_addr, header.nodeid, &metadata)
    }

    fn open(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        _in_body_addr: u64,
        out_addr: u64,
        _out_len: u32,
    ) -> u32 {
        let path = if header.nodeid == FUSE_ROOT_ID {
            match self.root_path() {
                Some(p) => p.to_path_buf(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        } else {
            match self.inodes.get(&header.nodeid) {
                Some(inode) => inode.path.clone(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        };

        let is_dir = path.is_dir();
        let fh = self.next_fh;
        self.next_fh += 1;
        self.file_handles.insert(fh, FileHandle { path, is_dir });

        let total = 32u32;
        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);
        ram.write_u64(out_addr + 16, fh);
        ram.write_u32(out_addr + 24, 0);
        ram.write_u32(out_addr + 28, 0);

        total
    }

    fn read(
        &self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        out_addr: u64,
        out_len: u32,
    ) -> u32 {
        let fh = ram.read_u64(in_body_addr);
        let offset = ram.read_u64(in_body_addr + 8);
        let size = ram.read_u32(in_body_addr + 16);

        let handle = match self.file_handles.get(&fh) {
            Some(h) => h,
            None => return self.reply_error(header, EBADF, out_addr, ram),
        };

        let mut file = match fs::File::open(&handle.path) {
            Ok(f) => f,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let _ = file.seek(std::io::SeekFrom::Start(offset));

        let max_read = size.min(out_len.saturating_sub(16)) as usize;
        let mut buf = vec![0u8; max_read];
        let bytes_read = match file.read(&mut buf) {
            Ok(n) => n,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let total = 16 + bytes_read as u32;
        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);
        ram.write_bytes(out_addr + 16, &buf[..bytes_read]);

        total
    }

    fn readdir(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        out_addr: u64,
        out_len: u32,
        plus: bool,
    ) -> u32 {
        let fh = ram.read_u64(in_body_addr);
        let offset = ram.read_u64(in_body_addr + 8);
        let size = ram.read_u32(in_body_addr + 16);

        let handle = match self.file_handles.get(&fh) {
            Some(h) => h,
            None => return self.reply_error(header, EBADF, out_addr, ram),
        };

        if !handle.is_dir {
            return self.reply_error(header, ENOTDIR, out_addr, ram);
        }

        let entries = match fs::read_dir(&handle.path) {
            Ok(rd) => rd,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let max_payload = size.min(out_len.saturating_sub(16)) as usize;
        let mut payload = Vec::with_capacity(max_payload);
        let mut entry_index: u64 = 0;

        // Add "." and ".."
        let dot_entries: Vec<(&str, u64)> = vec![(".", header.nodeid), ("..", 1)];

        for (name, ino) in &dot_entries {
            if entry_index < offset {
                entry_index += 1;
                continue;
            }
            let entry_size = self.dirent_size(name, plus);
            if payload.len() + entry_size > max_payload {
                break;
            }
            self.write_dirent(&mut payload, *ino, entry_index + 1, name, 4, plus);
            entry_index += 1;
        }

        let mut dir_entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        dir_entries.sort_by_key(|a| a.file_name());

        for entry in &dir_entries {
            if entry_index < offset {
                entry_index += 1;
                continue;
            }

            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            let entry_size = self.dirent_size(&name_str, plus);
            if payload.len() + entry_size > max_payload {
                break;
            }

            let metadata = match entry.metadata() {
                Ok(m) => m,
                Err(_) => {
                    entry_index += 1;
                    continue;
                }
            };

            let ino = self.get_or_create_inode_readonly(&entry.path(), metadata.is_dir());
            let file_type = if metadata.is_dir() { 4u32 } else { 8u32 };

            self.write_dirent(
                &mut payload,
                ino,
                entry_index + 1,
                &name_str,
                file_type,
                plus,
            );
            entry_index += 1;
        }

        let total = 16 + payload.len() as u32;
        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);
        ram.write_bytes(out_addr + 16, &payload);

        total
    }

    fn release(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        out_addr: u64,
        _out_len: u32,
    ) -> u32 {
        let fh = ram.read_u64(in_body_addr);
        self.file_handles.remove(&fh);

        let total = 16u32;
        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);

        total
    }

    fn statfs(
        &self,
        header: &FuseInHeader,
        out_addr: u64,
        _out_len: u32,
        ram: &mut RamView,
    ) -> u32 {
        let total = 16 + 80u32;
        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);

        let body = out_addr + 16;
        for i in 0..80 {
            ram.write_u8(body + i as u64, 0);
        }

        ram.write_u64(body, 1024 * 1024);
        ram.write_u64(body + 8, 512 * 1024);
        ram.write_u64(body + 16, 512 * 1024);
        ram.write_u64(body + 24, 1024 * 1024);
        ram.write_u64(body + 32, 512 * 1024);
        ram.write_u32(body + 40, 4096);
        ram.write_u32(body + 44, 255);
        ram.write_u32(body + 48, 4096);

        total
    }

    fn reply_error(
        &self,
        header: &FuseInHeader,
        error: i32,
        out_addr: u64,
        ram: &mut RamView,
    ) -> u32 {
        ram.write_u32(out_addr, 16);
        ram.write_u32(out_addr + 4, error as u32);
        ram.write_u64(out_addr + 8, header.unique);
        16
    }

    fn read_cstring(&self, ram: &RamView, addr: u64, max_len: u32) -> String {
        let mut bytes = Vec::new();
        for i in 0..max_len {
            let b = ram.read_u8(addr + i as u64);
            if b == 0 {
                break;
            }
            bytes.push(b);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn get_or_create_inode(&mut self, path: &Path, _is_dir: bool) -> u64 {
        for (&ino, inode) in &self.inodes {
            if inode.path == path {
                return ino;
            }
        }

        let ino = self.next_inode;
        self.next_inode += 1;
        self.inodes.insert(
            ino,
            Inode {
                path: path.to_path_buf(),
            },
        );

        ino
    }

    fn get_or_create_inode_readonly(&self, path: &Path, _is_dir: bool) -> u64 {
        for (&ino, inode) in &self.inodes {
            if inode.path == path {
                return ino;
            }
        }
        // temporary inode number for readdir
        0
    }

    fn write_entry_out(
        &self,
        header: &FuseInHeader,
        ram: &mut RamView,
        out_addr: u64,
        ino: u64,
        metadata: &fs::Metadata,
    ) -> u32 {
        let entry_out_size = 120u32;
        let total = 16 + entry_out_size;

        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);

        let body = out_addr + 16;
        ram.write_u64(body, ino);
        ram.write_u64(body + 8, 0);
        ram.write_u64(body + 16, 1);
        ram.write_u64(body + 24, 1);
        ram.write_u32(body + 32, 0);
        ram.write_u32(body + 36, 0);

        self.write_fuse_attr(ram, body + 40, ino, metadata);

        total
    }

    fn write_attr_out(
        &self,
        header: &FuseInHeader,
        ram: &mut RamView,
        out_addr: u64,
        ino: u64,
        metadata: &fs::Metadata,
    ) -> u32 {
        let attr_out_size = 104u32;
        let total = 16 + attr_out_size;

        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);

        let body = out_addr + 16;
        ram.write_u64(body, 1);
        ram.write_u32(body + 8, 0);
        ram.write_u32(body + 12, 0);

        self.write_fuse_attr(ram, body + 16, ino, metadata);

        total
    }

    fn write_fuse_attr(&self, ram: &mut RamView, addr: u64, ino: u64, metadata: &fs::Metadata) {
        let size = metadata.len();
        let is_dir = metadata.is_dir();
        let mode: u32 = if is_dir { 0o40755 } else { 0o100644 };
        let nlink: u32 = if is_dir { 2 } else { 1 };
        let blksize: u32 = 4096;
        let blocks = size.div_ceil(512);

        ram.write_u64(addr, ino);
        ram.write_u64(addr + 8, size);
        ram.write_u64(addr + 16, blocks);
        ram.write_u64(addr + 24, 0);
        ram.write_u64(addr + 32, 0);
        ram.write_u64(addr + 40, 0);
        ram.write_u32(addr + 48, 0);
        ram.write_u32(addr + 52, 0);
        ram.write_u32(addr + 56, 0);
        ram.write_u32(addr + 60, mode);
        ram.write_u32(addr + 64, nlink);
        ram.write_u32(addr + 68, 0);
        ram.write_u32(addr + 72, 0);
        ram.write_u32(addr + 76, 0);
        ram.write_u32(addr + 80, blksize);
        ram.write_u32(addr + 84, 0);
    }

    fn dirent_size(&self, name: &str, _plus: bool) -> usize {
        let base = 24 + name.len();
        let aligned = (base + 7) & !7;

        if _plus {
            aligned + 120
        } else {
            aligned
        }
    }

    fn write_dirent(
        &self,
        payload: &mut Vec<u8>,
        ino: u64,
        off: u64,
        name: &str,
        file_type: u32,
        _plus: bool,
    ) {
        if _plus {
            // For readdirplus
            payload.extend_from_slice(&[0u8; 120]);

            let start = payload.len() - 120;
            payload[start..start + 8].copy_from_slice(&ino.to_le_bytes());
        }

        payload.extend_from_slice(&ino.to_le_bytes());
        payload.extend_from_slice(&off.to_le_bytes());
        payload.extend_from_slice(&(name.len() as u32).to_le_bytes());
        payload.extend_from_slice(&file_type.to_le_bytes());
        payload.extend_from_slice(name.as_bytes());

        let padding = (8 - (name.len() % 8)) % 8;
        payload.extend(std::iter::repeat_n(0u8, padding));
    }
}
