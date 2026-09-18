// See FUSE protocol docs to have more information on ram writes/reads

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{STAGING_BASE, RamView, VRING_DESC_F_NEXT, VRING_DESC_F_WRITE, VirtioMmio};
use crate::trace::Tracer;

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
const FUSE_WRITE: u32 = 16;
const FUSE_CREATE: u32 = 35;
const FUSE_MKDIR: u32 = 9;
const FUSE_RMDIR: u32 = 11;
const FUSE_UNLINK: u32 = 10;
const FUSE_RENAME: u32 = 12;
const FUSE_RENAME2: u32 = 45;
const FUSE_SETATTR: u32 = 4;
const FUSE_FLUSH: u32 = 25;
const FUSE_FSYNC: u32 = 20;
const FUSE_FSYNCDIR: u32 = 30;

const ENOENT: i32 = -2;
const ENOTDIR: i32 = -20;
const ENOSYS: i32 = -38;
const EBADF: i32 = -9;
const EACCES: i32 = -13;
const ENOTEMPTY: i32 = -39;
const EEXIST: i32 = -17;
const EINVAL: i32 = -22;

// const FATTR_MODE: u32 = 1 << 0;
const FATTR_SIZE: u32 = 1 << 3;

const FUSE_ROOT_ID: u64 = 1;

const O_ACCMODE: u32 = 0o3;
const O_WRONLY: u32 = 0o1;
const O_RDWR: u32 = 0o2;
const O_TRUNC: u32 = 0o1000;

// #[repr(C)]
struct FuseInHeader {
    _len: u32,
    opcode: u32,
    unique: u64,
    nodeid: u64,
    _uid: u32,
    _gid: u32,
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

#[derive(Clone)]
pub struct Mount {
    pub host_path: PathBuf,
    pub tag: String,
    pub writable: bool,
}

pub struct VirtioFs {
    pub mmio: VirtioMmio,
    inodes: HashMap<u64, Inode>,
    next_inode: u64,
    file_handles: HashMap<u64, FileHandle>,
    next_fh: u64,
    mounts: Vec<Mount>,
    tracer: Option<Tracer>,
    guest_root: String,
    traced_handles: HashMap<u64, TracedHandle>,
}

enum MountRequest {
    Open { path: String, flags: u32 },
    Read { handle: u64 },
    Write { handle: u64 },
    Release { handle: u64 },
    Create { path: String },
    Mkdir { path: String },
    Delete { path: String, directory: bool },
    Rename { from: String, to: String },
    Truncate { path: String, size: u64 },
}

struct TracedHandle {
    path: String,
    bytes_read: u64,
    bytes_written: u64,
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
            tracer: None,
            guest_root: String::new(),
            traced_handles: HashMap::new(),
        };

        let tag = b"virtiofs";
        device.mmio.config[..tag.len()].copy_from_slice(tag);
        device.mmio.config[36..40].copy_from_slice(&1u32.to_le_bytes());

        device
    }

    pub fn new_single(mount: Mount, tag: &str) -> Self {
        let mut device = Self {
            mmio: VirtioMmio::new(DEVICE_ID, DEVICE_FEATURES, 2),
            inodes: HashMap::new(),
            next_inode: FUSE_ROOT_ID + 1,
            file_handles: HashMap::new(),
            next_fh: 1,
            mounts: vec![mount],
            tracer: None,
            guest_root: String::new(),
            traced_handles: HashMap::new(),
        };

        let tag_bytes = tag.as_bytes();
        let len = tag_bytes.len().min(36);
        device.mmio.config[..len].copy_from_slice(&tag_bytes[..len]);
        device.mmio.config[36..40].copy_from_slice(&1u32.to_le_bytes());

        device
    }

    pub fn set_mounts(&mut self, mounts: Vec<Mount>) {
        self.mounts = mounts;
    }

    pub fn set_guest_root(&mut self, guest_root: &str) {
        self.guest_root = guest_root.trim_end_matches('/').to_string();
    }

    pub fn set_tracer(&mut self, tracer: Option<Tracer>) {
        self.tracer = tracer.filter(|tracer| tracer.traces_mounts());
        self.traced_handles.clear();
    }

    fn root_path(&self) -> Option<&Path> {
        self.mounts.first().and_then(|m| {
            if m.host_path.as_os_str().is_empty() {
                None
            } else {
                Some(m.host_path.as_path())
            }
        })
    }

    fn is_writable(&self) -> bool {
        self.mounts.first().is_some_and(|m| m.writable)
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
            _len: ram.read_u32(header_addr),
            opcode: ram.read_u32(header_addr + 4),
            unique: ram.read_u64(header_addr + 8),
            nodeid: ram.read_u64(header_addr + 16),
            _uid: ram.read_u32(header_addr + 24),
            _gid: ram.read_u32(header_addr + 28),
            pid: ram.read_u32(header_addr + 32),
            _padding: ram.read_u32(header_addr + 36),
        };

        let (in_body_addr, in_body_len) = if header_len > 40 {
            (header_addr + 40, header_len - 40)
        } else if read_bufs.len() > 1 {
            (read_bufs[1].0, read_bufs[1].1)
        } else {
            (header_addr + 40, 0)
        };

        let capacity: u32 = write_bufs.iter().map(|(_, len)| *len).sum();
        let (contiguous_addr, contiguous_len) = contiguous_reply_window(&write_bufs);

        let staged = header.opcode != FUSE_READ && contiguous_len < capacity;
        let (out_addr, out_len) = if staged {
            ram.begin_staging(STAGING_BASE, capacity as usize);
            (STAGING_BASE, capacity)
        } else {
            (contiguous_addr, contiguous_len)
        };

        let traced_request = match self.tracer {
            Some(_) => self.describe_request(&header, ram, in_body_addr, in_body_len),
            None => None,
        };

        let used_len = match header.opcode {
            FUSE_INIT => self.init(&header, out_addr, out_len, ram),
            FUSE_LOOKUP => self.lookup(&header, ram, in_body_addr, in_body_len, out_addr, out_len),
            FUSE_GETATTR => self.getattr(&header, out_addr, out_len, ram),
            FUSE_OPEN | FUSE_OPENDIR => self.open(&header, ram, in_body_addr, out_addr, out_len),
            FUSE_READ => self.read(&header, ram, in_body_addr, &write_bufs),
            FUSE_READDIR => self.readdir(&header, ram, in_body_addr, out_addr, out_len, false),
            FUSE_READDIRPLUS => self.readdir(&header, ram, in_body_addr, out_addr, out_len, true),
            FUSE_RELEASE | FUSE_RELEASEDIR => {
                self.release(&header, ram, in_body_addr, out_addr, out_len)
            }
            FUSE_STATFS => self.statfs(&header, out_addr, out_len, ram),
            FUSE_FORGET | FUSE_BATCH_FORGET => return 0,
            FUSE_WRITE => self.write_data(&header, ram, in_body_addr, &read_bufs, out_addr),
            FUSE_CREATE => self.create(&header, ram, in_body_addr, in_body_len, out_addr, out_len),
            FUSE_MKDIR => self.mkdir(&header, ram, in_body_addr, in_body_len, out_addr, out_len),
            FUSE_RMDIR => self.rmdir(&header, ram, in_body_addr, in_body_len, out_addr),
            FUSE_UNLINK => self.unlink(&header, ram, in_body_addr, in_body_len, out_addr),
            FUSE_RENAME | FUSE_RENAME2 => {
                self.rename(&header, ram, in_body_addr, in_body_len, out_addr)
            }
            FUSE_SETATTR => self.setattr(&header, ram, in_body_addr, out_addr, out_len),
            FUSE_FLUSH | FUSE_FSYNC | FUSE_FSYNCDIR => self.flush(&header, out_addr, ram),
            _ => self.reply_error(&header, ENOSYS, out_addr, ram),
        };

        if let Some(request) = traced_request
            && used_len >= 16
        {
            self.record_request(request, &header, ram, out_addr, used_len);
        }

        if let Some(reply) = ram.take_staging() {
            let written = (used_len as usize).min(reply.len());
            scatter_write(ram, &write_bufs, 0, &reply[..written]);
        }

        used_len
    }

    fn guest_path_of_node(&self, nodeid: u64) -> Option<String> {
        if nodeid == FUSE_ROOT_ID {
            return Some(if self.guest_root.is_empty() {
                "/".to_string()
            } else {
                self.guest_root.clone()
            });
        }

        let host_path = &self.inodes.get(&nodeid)?.path;
        let relative = host_path.strip_prefix(self.root_path()?).ok()?;
        Some(format!(
            "{}/{}",
            self.guest_root,
            relative.to_string_lossy()
        ))
    }

    fn guest_path_of_child(&self, parent: u64, name: &str) -> Option<String> {
        let parent = self.guest_path_of_node(parent)?;
        Some(format!("{}/{name}", parent.trim_end_matches('/')))
    }

    fn describe_request(
        &self,
        header: &FuseInHeader,
        ram: &RamView,
        in_body_addr: u64,
        in_body_len: u32,
    ) -> Option<MountRequest> {
        let name_at = |offset: u32| {
            self.read_cstring(
                ram,
                in_body_addr + offset as u64,
                in_body_len.saturating_sub(offset),
            )
        };

        Some(match header.opcode {
            FUSE_OPEN => MountRequest::Open {
                path: self.guest_path_of_node(header.nodeid)?,
                flags: ram.read_u32(in_body_addr),
            },
            FUSE_READ => MountRequest::Read {
                handle: ram.read_u64(in_body_addr),
            },
            FUSE_WRITE => MountRequest::Write {
                handle: ram.read_u64(in_body_addr),
            },
            FUSE_RELEASE => MountRequest::Release {
                handle: ram.read_u64(in_body_addr),
            },
            FUSE_CREATE => MountRequest::Create {
                path: self.guest_path_of_child(header.nodeid, &name_at(16))?,
            },
            FUSE_MKDIR => MountRequest::Mkdir {
                path: self.guest_path_of_child(header.nodeid, &name_at(8))?,
            },
            FUSE_UNLINK | FUSE_RMDIR => MountRequest::Delete {
                path: self.guest_path_of_child(header.nodeid, &name_at(0))?,
                directory: header.opcode == FUSE_RMDIR,
            },
            FUSE_RENAME | FUSE_RENAME2 => {
                let names_at = if header.opcode == FUSE_RENAME2 { 12 } else { 8 };
                let from_name = name_at(names_at);
                let to_name = name_at(names_at + from_name.len() as u32 + 1);
                MountRequest::Rename {
                    from: self.guest_path_of_child(header.nodeid, &from_name)?,
                    to: self.guest_path_of_child(ram.read_u64(in_body_addr), &to_name)?,
                }
            }
            FUSE_SETATTR if ram.read_u32(in_body_addr) & FATTR_SIZE != 0 => {
                MountRequest::Truncate {
                    path: self.guest_path_of_node(header.nodeid)?,
                    size: ram.read_u64(in_body_addr + 16),
                }
            }
            _ => return None,
        })
    }

    fn record_request(
        &mut self,
        request: MountRequest,
        header: &FuseInHeader,
        ram: &RamView,
        out_addr: u64,
        used_len: u32,
    ) {
        let Some(tracer) = self.tracer.clone() else {
            return;
        };
        let result = ram.read_u32(out_addr + 4) as i32;
        let succeeded = result == 0;
        let pid = Value::from(header.pid);

        match request {
            MountRequest::Read { handle } => {
                if succeeded && let Some(traced) = self.traced_handles.get_mut(&handle) {
                    traced.bytes_read += (used_len - 16) as u64;
                }
            }
            MountRequest::Write { handle } => {
                if succeeded && let Some(traced) = self.traced_handles.get_mut(&handle) {
                    traced.bytes_written += ram.read_u32(out_addr + 16) as u64;
                }
            }
            MountRequest::Release { handle } => {
                if let Some(traced) = self.traced_handles.remove(&handle) {
                    tracer.record(
                        "mount.close",
                        &[
                            ("pid", pid),
                            ("path", traced.path.into()),
                            ("bytes_read", traced.bytes_read.into()),
                            ("bytes_written", traced.bytes_written.into()),
                        ],
                    );
                }
            }
            MountRequest::Open { path, flags } => {
                let access = match flags & O_ACCMODE {
                    O_WRONLY => "write",
                    O_RDWR => "read-write",
                    _ => "read",
                };
                if succeeded {
                    self.track_handle(ram.read_u64(out_addr + 16), &path);
                }
                tracer.record(
                    "mount.open",
                    &[
                        ("pid", pid),
                        ("path", path.into()),
                        ("access", access.into()),
                        ("truncate", (flags & O_TRUNC != 0).into()),
                        ("result", result.into()),
                    ],
                );
            }
            MountRequest::Create { path } => {
                if succeeded {
                    self.track_handle(ram.read_u64(out_addr + 16 + 128), &path);
                }
                tracer.record(
                    "mount.create",
                    &[
                        ("pid", pid),
                        ("path", path.into()),
                        ("result", result.into()),
                    ],
                );
            }
            MountRequest::Mkdir { path } => tracer.record(
                "mount.mkdir",
                &[
                    ("pid", pid),
                    ("path", path.into()),
                    ("result", result.into()),
                ],
            ),
            MountRequest::Delete { path, directory } => tracer.record(
                "mount.delete",
                &[
                    ("pid", pid),
                    ("path", path.into()),
                    ("directory", directory.into()),
                    ("result", result.into()),
                ],
            ),
            MountRequest::Rename { from, to } => tracer.record(
                "mount.rename",
                &[
                    ("pid", pid),
                    ("from", from.into()),
                    ("to", to.into()),
                    ("result", result.into()),
                ],
            ),
            MountRequest::Truncate { path, size } => tracer.record(
                "mount.truncate",
                &[
                    ("pid", pid),
                    ("path", path.into()),
                    ("size", size.into()),
                    ("result", result.into()),
                ],
            ),
        }
    }

    fn track_handle(&mut self, handle: u64, path: &str) {
        self.traced_handles.insert(
            handle,
            TracedHandle {
                path: path.to_string(),
                bytes_read: 0,
                bytes_written: 0,
            },
        );
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
        write_bufs: &[(u64, u32)],
    ) -> u32 {
        let fh = ram.read_u64(in_body_addr);
        let offset = ram.read_u64(in_body_addr + 8);
        let size = ram.read_u32(in_body_addr + 16);
        let out_addr = write_bufs[0].0;

        let capacity: u64 = write_bufs.iter().map(|(_, len)| *len as u64).sum();

        let handle = match self.file_handles.get(&fh) {
            Some(h) => h,
            None => return self.reply_error(header, EBADF, out_addr, ram),
        };

        let mut file = match fs::File::open(&handle.path) {
            Ok(f) => f,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let _ = file.seek(std::io::SeekFrom::Start(offset));

        let max_read = (size as u64).min(capacity.saturating_sub(16)) as usize;
        let mut buf = vec![0u8; max_read];
        let bytes_read = match read_filling(&mut file, &mut buf) {
            Ok(n) => n,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let total = 16 + bytes_read as u32;
        let mut out_header = [0u8; 16];
        out_header[..4].copy_from_slice(&total.to_le_bytes());
        out_header[8..].copy_from_slice(&header.unique.to_le_bytes());

        scatter_write(ram, write_bufs, 0, &out_header);
        scatter_write(ram, write_bufs, 16, &buf[..bytes_read]);

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
        let entry_out_size = 128u32;
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

        if _plus { aligned + 128 } else { aligned }
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
            payload.extend_from_slice(&[0u8; 128]);
            let start = payload.len() - 128;
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

    fn write_data(
        &self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        read_bufs: &[(u64, u32)],
        out_addr: u64,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let fh = ram.read_u64(in_body_addr);
        let offset = ram.read_u64(in_body_addr + 8);
        let size = ram.read_u32(in_body_addr + 16);

        let handle = match self.file_handles.get(&fh) {
            Some(h) => h,
            None => return self.reply_error(header, EBADF, out_addr, ram),
        };

        let mut file = match fs::OpenOptions::new().write(true).open(&handle.path) {
            Ok(f) => f,
            Err(_) => return self.reply_error(header, EACCES, out_addr, ram),
        };

        if file.seek(std::io::SeekFrom::Start(offset)).is_err() {
            return self.reply_error(header, EINVAL, out_addr, ram);
        }

        let in_body_buf_idx = read_bufs
            .iter()
            .position(|&(addr, _)| addr == in_body_addr)
            .unwrap_or(0);

        let write_in_size: u64 = 40;
        let data_addr = in_body_addr + write_in_size;
        let in_body_buf_end = read_bufs[in_body_buf_idx].0 + read_bufs[in_body_buf_idx].1 as u64;
        let inline_avail = in_body_buf_end.saturating_sub(data_addr) as u32;

        let mut written: u32 = 0;

        if inline_avail > 0 {
            let chunk = inline_avail.min(size) as usize;
            let mut data = vec![0u8; chunk];
            ram.read_bytes(data_addr, &mut data);
            if file.write_all(&data).is_ok() {
                written += chunk as u32;
            }
        }

        let mut remaining = size.saturating_sub(written);
        for &(buf_addr, buf_len) in read_bufs.iter().skip(in_body_buf_idx + 1) {
            if remaining == 0 {
                break;
            }
            let chunk = buf_len.min(remaining) as usize;
            let mut data = vec![0u8; chunk];
            ram.read_bytes(buf_addr, &mut data);
            match file.write_all(&data) {
                Ok(_) => written += chunk as u32,
                Err(_) => break,
            }
            remaining -= chunk as u32;
        }

        let total = 24u32;
        ram.write_u32(out_addr, total);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);
        ram.write_u32(out_addr + 16, written);
        ram.write_u32(out_addr + 20, 0);

        total
    }

    fn create(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        in_body_len: u32,
        out_addr: u64,
        _out_len: u32,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let name_offset = 16u32;
        let name = self.read_cstring(
            ram,
            in_body_addr + name_offset as u64,
            in_body_len.saturating_sub(name_offset),
        );

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

        let file_path = parent_path.join(&name);

        if fs::File::create(&file_path).is_err() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let metadata = match fs::metadata(&file_path) {
            Ok(m) => m,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let ino = self.get_or_create_inode(&file_path, false);

        let fh = self.next_fh;
        self.next_fh += 1;
        self.file_handles.insert(
            fh,
            FileHandle {
                path: file_path,
                is_dir: false,
            },
        );

        let total = 16 + 128 + 16;
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
        self.write_fuse_attr(ram, body + 40, ino, &metadata);

        let open_out = out_addr + 16 + 128;
        ram.write_u64(open_out, fh);
        ram.write_u32(open_out + 8, 0);
        ram.write_u32(open_out + 12, 0);

        total
    }

    fn mkdir(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        in_body_len: u32,
        out_addr: u64,
        _out_len: u32,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let name = self.read_cstring(ram, in_body_addr + 8, in_body_len.saturating_sub(8));

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

        let dir_path = parent_path.join(&name);

        if dir_path.exists() {
            return self.reply_error(header, EEXIST, out_addr, ram);
        }

        if fs::create_dir(&dir_path).is_err() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let metadata = match fs::metadata(&dir_path) {
            Ok(m) => m,
            Err(_) => return self.reply_error(header, ENOENT, out_addr, ram),
        };

        let ino = self.get_or_create_inode(&dir_path, true);
        self.write_entry_out(header, ram, out_addr, ino, &metadata)
    }

    fn rmdir(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        in_body_len: u32,
        out_addr: u64,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

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

        let dir_path = parent_path.join(&name);

        match fs::remove_dir(&dir_path) {
            Ok(_) => self.reply_ok(header, out_addr, ram),
            Err(e) => {
                let errno = if e.raw_os_error() == Some(39) {
                    ENOTEMPTY
                } else {
                    ENOENT
                };
                self.reply_error(header, errno, out_addr, ram)
            }
        }
    }

    fn unlink(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        in_body_len: u32,
        out_addr: u64,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

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

        let file_path = parent_path.join(&name);

        match fs::remove_file(&file_path) {
            Ok(_) => self.reply_ok(header, out_addr, ram),
            Err(_) => self.reply_error(header, ENOENT, out_addr, ram),
        }
    }

    fn rename(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        in_body_len: u32,
        out_addr: u64,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let newdir = ram.read_u64(in_body_addr);
        let name_start = if header.opcode == FUSE_RENAME2 {
            12u32
        } else {
            8u32
        };

        let old_name = self.read_cstring(
            ram,
            in_body_addr + name_start as u64,
            in_body_len.saturating_sub(name_start),
        );
        let new_name_offset = name_start + old_name.len() as u32 + 1;
        let new_name = self.read_cstring(
            ram,
            in_body_addr + new_name_offset as u64,
            in_body_len.saturating_sub(new_name_offset),
        );

        let old_parent = if header.nodeid == FUSE_ROOT_ID {
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

        let new_parent = if newdir == FUSE_ROOT_ID {
            match self.root_path() {
                Some(p) => p.to_path_buf(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        } else {
            match self.inodes.get(&newdir) {
                Some(inode) => inode.path.clone(),
                None => return self.reply_error(header, ENOENT, out_addr, ram),
            }
        };

        let old_path = old_parent.join(&old_name);
        let new_path = new_parent.join(&new_name);

        match fs::rename(&old_path, &new_path) {
            Ok(_) => {
                for inode in self.inodes.values_mut() {
                    if inode.path == old_path {
                        inode.path = new_path.clone();
                        break;
                    }
                }
                self.reply_ok(header, out_addr, ram)
            }
            Err(_) => self.reply_error(header, EACCES, out_addr, ram),
        }
    }

    fn setattr(
        &mut self,
        header: &FuseInHeader,
        ram: &mut RamView,
        in_body_addr: u64,
        out_addr: u64,
        out_len: u32,
    ) -> u32 {
        if !self.is_writable() {
            return self.reply_error(header, EACCES, out_addr, ram);
        }

        let valid = ram.read_u32(in_body_addr);

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

        if valid & FATTR_SIZE != 0 {
            let new_size = ram.read_u64(in_body_addr + 16);
            let file = match fs::OpenOptions::new().write(true).open(&path) {
                Ok(f) => f,
                Err(_) => return self.reply_error(header, EACCES, out_addr, ram),
            };
            if file.set_len(new_size).is_err() {
                return self.reply_error(header, EACCES, out_addr, ram);
            }
        }

        // if valid & FATTR_MODE != 0 {
        //  WASI doesn't support chmod yet
        // }

        self.getattr(header, out_addr, out_len, ram)
    }

    fn flush(&self, header: &FuseInHeader, out_addr: u64, ram: &mut RamView) -> u32 {
        self.reply_ok(header, out_addr, ram)
    }

    fn reply_ok(&self, header: &FuseInHeader, out_addr: u64, ram: &mut RamView) -> u32 {
        ram.write_u32(out_addr, 16);
        ram.write_u32(out_addr + 4, 0);
        ram.write_u64(out_addr + 8, header.unique);
        16
    }
}

fn contiguous_reply_window(write_bufs: &[(u64, u32)]) -> (u64, u32) {
    let (first_addr, first_len) = write_bufs[0];
    let mut len = first_len;

    for &(addr, buf_len) in &write_bufs[1..] {
        if addr != first_addr + len as u64 {
            break;
        }
        len = len.saturating_add(buf_len);
    }

    (first_addr, len)
}

fn scatter_write(ram: &mut RamView, write_bufs: &[(u64, u32)], offset: u64, data: &[u8]) {
    let mut remaining = data;
    let mut reply_offset = 0u64;

    for &(addr, len) in write_bufs {
        if remaining.is_empty() {
            return;
        }

        let len = len as u64;
        let buffer_end = reply_offset + len;

        if buffer_end > offset {
            let skip = offset.saturating_sub(reply_offset);
            let room = (len - skip) as usize;
            let take = room.min(remaining.len());

            ram.write_bytes(addr + skip, &remaining[..take]);
            remaining = &remaining[take..];
        }

        reply_offset = buffer_end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RAM_BASE;
    use crate::cow_ram::CowRam;

    const RAM_BYTES: u64 = 1024 * 1024;

    fn ram() -> CowRam {
        CowRam::new(RAM_BYTES)
    }

    #[test]
    fn a_reply_spans_buffers_the_guest_scattered_across_memory() {
        let mut backing = ram();
        let mut view = RamView::new(&mut backing, RAM_BYTES - 1);

        let first = RAM_BASE + 0x1000;
        let second = RAM_BASE + 0x9000;
        let bufs = [(first, 16u32), (second, 32u32)];

        let header: Vec<u8> = (0..16).collect();
        let data: Vec<u8> = (100..120).collect();

        scatter_write(&mut view, &bufs, 0, &header);
        scatter_write(&mut view, &bufs, 16, &data);

        let mut landed = vec![0u8; 16];
        view.read_bytes(first, &mut landed);
        assert_eq!(landed, header);

        let mut payload = vec![0u8; data.len()];
        view.read_bytes(second, &mut payload);
        assert_eq!(payload, data);
    }

    #[test]
    fn a_reply_longer_than_the_chain_stops_at_the_last_buffer() {
        let mut backing = ram();
        let mut view = RamView::new(&mut backing, RAM_BYTES - 1);

        let only = RAM_BASE + 0x2000;
        let bufs = [(only, 8u32)];
        let guard = only + 8;
        view.write_u8(guard, 0xAB);

        scatter_write(&mut view, &bufs, 0, &[1u8; 64]);

        assert_eq!(view.read_u8(guard), 0xAB, "wrote past the guest's buffer");
    }

    #[test]
    fn a_split_payload_crosses_the_boundary_between_two_buffers() {
        let mut backing = ram();
        let mut view = RamView::new(&mut backing, RAM_BYTES - 1);

        let first = RAM_BASE + 0x3000;
        let second = RAM_BASE + 0xB000;
        let bufs = [(first, 4u32), (second, 4u32)];

        scatter_write(&mut view, &bufs, 0, &[1, 2, 3, 4, 5, 6, 7, 8]);

        let mut head = vec![0u8; 4];
        let mut tail = vec![0u8; 4];
        view.read_bytes(first, &mut head);
        view.read_bytes(second, &mut tail);

        assert_eq!(head, [1, 2, 3, 4]);
        assert_eq!(tail, [5, 6, 7, 8]);
    }

    #[test]
    fn a_staged_reply_never_touches_guest_memory_until_it_is_scattered() {
        let mut backing = ram();
        let mut view = RamView::new(&mut backing, RAM_BYTES - 1);

        let guest = RAM_BASE + 0x6000;
        view.write_u32(guest, 0xDEAD_BEEF);

        view.begin_staging(STAGING_BASE, 64);
        view.write_u32(STAGING_BASE, 16);
        view.write_u64(STAGING_BASE + 8, 0x1122_3344);

        assert_eq!(view.read_u32(STAGING_BASE), 16);
        assert_eq!(view.read_u32(guest), 0xDEAD_BEEF, "guest memory was disturbed");

        let staged = view.take_staging().expect("staging was started");
        assert_eq!(&staged[..4], &16u32.to_le_bytes());
        assert!(view.take_staging().is_none());
    }

    #[test]
    fn touching_buffers_count_as_one_window_and_separated_ones_do_not() {
        let base = RAM_BASE + 0x4000;

        assert_eq!(
            contiguous_reply_window(&[(base, 16), (base + 16, 48), (base + 64, 8)]),
            (base, 72)
        );
        assert_eq!(
            contiguous_reply_window(&[(base, 16), (base + 0x5000, 48)]),
            (base, 16)
        );
    }
}

fn read_filling(file: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;

    while filled < buf.len() {
        match file.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(count) => filled += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }

    Ok(filled)
}
