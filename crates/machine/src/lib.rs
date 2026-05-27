pub mod clint;
pub mod dtb;
pub mod plic;
pub mod snapshot;
pub mod uart;
pub mod virtio;
pub mod machine_bus;

pub const RAM_BASE: u64 = 0x8000_0000;
pub const UART_BASE: u64 = 0x1000_0000;
pub const UART_SIZE: u64 = 0x100;
pub const UART_IRQ: u32 = 1;

pub const VIRTIO_BASE: u64 = 0x1000_1000;
pub const VIRTIO_SIZE: u64 = 0x200;
pub const VIRTIO_BLK_IRQ: u32 = 2;
pub const VIRTIO_CONSOLE_IRQ: u32 = 3;
pub const VIRTIO_NET_IRQ: u32 = 4;

pub const GUEST_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

pub const KERNEL_OFFSET: u64 = 0x20_0000; // text_offset (2 MB)

pub const LOW_RAM_BASE: u64 = 0x0000_0000;
pub const LOW_RAM_SIZE: u64 = 0x0001_0000;
