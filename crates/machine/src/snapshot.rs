use std::io::{self, Read, Write};

use riscv_core::Hart;
use riscv_core::csr::PrivMode;

use crate::LOW_RAM_SIZE;
use crate::machine_bus::MachineBus;

const MAGIC: &[u8; 4] = b"VPOD";
const VERSION: u8 = 1;

pub fn save(bus: &MachineBus, hart: &Hart, writer: &mut impl Write) -> io::Result<()> {
    writer.write_all(MAGIC)?;
    writer.write_all(&[VERSION])?;

    let ram_size = bus.ram_size();
    writer.write_all(&ram_size.to_le_bytes())?;
    writer.write_all(&bus.ram)?;
    writer.write_all(&bus.low_ram)?;

    save_hart(hart, writer)?;
    bus.clint.serialize(writer)?;
    bus.plic.save_state(writer)?;
    bus.uart.serialize(writer)?;

    bus.console.mmio.serialize(writer)?;

    let has_net = bus.net.is_some();
    writer.write_all(&[has_net as u8])?;
    if let Some(net) = &bus.net {
        net.mmio.serialize(writer)?;
    }

    Ok(())
}

pub fn restore(bus: &mut MachineBus, hart: &mut Hart, reader: &mut impl Read) -> io::Result<()> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;

    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid snapshot magic",
        ));
    }

    let mut version = [0u8; 1];
    reader.read_exact(&mut version)?;

    if version[0] != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported snapshot version {}", version[0]),
        ));
    }

    let mut buffer_u64 = [0u8; 8];
    reader.read_exact(&mut buffer_u64)?;

    let ram_size = u64::from_le_bytes(buffer_u64);
    if ram_size != bus.ram_size() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "snapshot RAM size {} != current {}",
                ram_size,
                bus.ram_size()
            ),
        ));
    }

    reader.read_exact(&mut bus.ram)?;

    let mut low = vec![0u8; LOW_RAM_SIZE as usize];
    reader.read_exact(&mut low)?;
    bus.low_ram = low;

    restore_hart(hart, reader)?;
    bus.clint.deserialize(reader)?;
    bus.plic.restore_state(reader)?;
    bus.uart.deserialize(reader)?;

    bus.console.mmio.deserialize(reader)?;

    let mut has_net = [0u8; 1];
    reader.read_exact(&mut has_net)?;
    if has_net[0] != 0 {
        if let Some(net) = &mut bus.net {
            net.mmio.deserialize(reader)?;
        } else {
            let mut skip = crate::virtio::VirtioMmio::new(0, 0, 2);
            skip.deserialize(reader)?;
        }
    }

    Ok(())
}

fn save_hart(hart: &Hart, writer: &mut impl Write) -> io::Result<()> {
    for i in 0..32usize {
        writer.write_all(&hart.regs.read(i).to_le_bytes())?;
    }

    writer.write_all(&hart.regs.pc.to_le_bytes())?;
    for i in 0..32usize {
        writer.write_all(&hart.regs.read_f(i).to_le_bytes())?;
    }

    let privilege_mode: u8 = match hart.priv_mode {
        PrivMode::U => 0,
        PrivMode::S => 1,
        PrivMode::M => 3,
    };
    writer.write_all(&[privilege_mode])?;

    match hart.lr_addr {
        None => writer.write_all(&[0u8])?,
        Some(address) => {
            writer.write_all(&[1u8])?;
            writer.write_all(&address.to_le_bytes())?;
        }
    }

    writer.write_all(&hart.fetch_vpage.to_le_bytes())?;
    writer.write_all(&hart.fetch_ppage.to_le_bytes())?;
    writer.write_all(&hart.fetch_satp.to_le_bytes())?;

    save_csr(hart, writer)
}

fn restore_hart(hart: &mut Hart, reader: &mut impl Read) -> io::Result<()> {
    let mut buffer_u64 = [0u8; 8];

    for i in 0..32usize {
        reader.read_exact(&mut buffer_u64)?;
        hart.regs.write(i, u64::from_le_bytes(buffer_u64));
    }

    reader.read_exact(&mut buffer_u64)?;
    hart.regs.pc = u64::from_le_bytes(buffer_u64);

    for i in 0..32usize {
        reader.read_exact(&mut buffer_u64)?;
        hart.regs.write_f(i, u64::from_le_bytes(buffer_u64));
    }

    let mut buffer_u8 = [0u8; 1];
    reader.read_exact(&mut buffer_u8)?;
    hart.priv_mode = match buffer_u8[0] {
        0 => PrivMode::U,
        1 => PrivMode::S,
        _ => PrivMode::M,
    };

    reader.read_exact(&mut buffer_u8)?;
    hart.lr_addr = if buffer_u8[0] == 0 {
        None
    } else {
        reader.read_exact(&mut buffer_u64)?;
        Some(u64::from_le_bytes(buffer_u64))
    };

    reader.read_exact(&mut buffer_u64)?;
    hart.fetch_vpage = u64::from_le_bytes(buffer_u64);

    reader.read_exact(&mut buffer_u64)?;
    hart.fetch_ppage = u64::from_le_bytes(buffer_u64);

    reader.read_exact(&mut buffer_u64)?;
    hart.fetch_satp = u64::from_le_bytes(buffer_u64);

    restore_csr(hart, reader)
}

fn save_csr(hart: &Hart, writer: &mut impl Write) -> io::Result<()> {
    let csr_state = &hart.csr;

    macro_rules! write_u64 {
        ($f:expr) => {
            writer.write_all(&$f.to_le_bytes())?
        };
    }
    write_u64!(csr_state.mstatus);
    write_u64!(csr_state.misa);
    write_u64!(csr_state.medeleg);
    write_u64!(csr_state.mideleg);
    write_u64!(csr_state.mie);
    write_u64!(csr_state.mtvec);
    write_u64!(csr_state.mcounteren);
    write_u64!(csr_state.mcountinhibit);
    write_u64!(csr_state.mscratch);
    write_u64!(csr_state.mepc);
    write_u64!(csr_state.mcause);
    write_u64!(csr_state.mtval);
    write_u64!(csr_state.mip);
    write_u64!(csr_state.menvcfg);
    write_u64!(csr_state.stvec);
    write_u64!(csr_state.scounteren);
    write_u64!(csr_state.senvcfg);
    write_u64!(csr_state.sscratch);
    write_u64!(csr_state.sepc);
    write_u64!(csr_state.scause);
    write_u64!(csr_state.stval);
    write_u64!(csr_state.satp);
    write_u64!(csr_state.cycle);
    write_u64!(csr_state.instret);
    write_u64!(csr_state.time);
    write_u64!(csr_state.fcsr);

    for value in &csr_state.pmpcfg {
        write_u64!(value);
    }
    for value in &csr_state.pmpaddr {
        write_u64!(value);
    }
    for value in &csr_state.mhpmevent {
        write_u64!(value);
    }

    Ok(())
}

fn restore_csr(hart: &mut Hart, reader: &mut impl Read) -> io::Result<()> {
    let csr_state = &mut hart.csr;
    let mut buffer = [0u8; 8];

    macro_rules! read_u64 {
        ($f:expr) => {
            reader.read_exact(&mut buffer)?;
            $f = u64::from_le_bytes(buffer);
        };
    }
    read_u64!(csr_state.mstatus);
    read_u64!(csr_state.misa);
    read_u64!(csr_state.medeleg);
    read_u64!(csr_state.mideleg);
    read_u64!(csr_state.mie);
    read_u64!(csr_state.mtvec);
    read_u64!(csr_state.mcounteren);
    read_u64!(csr_state.mcountinhibit);
    read_u64!(csr_state.mscratch);
    read_u64!(csr_state.mepc);
    read_u64!(csr_state.mcause);
    read_u64!(csr_state.mtval);
    read_u64!(csr_state.mip);
    read_u64!(csr_state.menvcfg);
    read_u64!(csr_state.stvec);
    read_u64!(csr_state.scounteren);
    read_u64!(csr_state.senvcfg);
    read_u64!(csr_state.sscratch);
    read_u64!(csr_state.sepc);
    read_u64!(csr_state.scause);
    read_u64!(csr_state.stval);
    read_u64!(csr_state.satp);
    read_u64!(csr_state.cycle);
    read_u64!(csr_state.instret);
    read_u64!(csr_state.time);
    read_u64!(csr_state.fcsr);

    for value in csr_state.pmpcfg.iter_mut() {
        reader.read_exact(&mut buffer)?;
        *value = u64::from_le_bytes(buffer);
    }

    for value in csr_state.pmpaddr.iter_mut() {
        reader.read_exact(&mut buffer)?;
        *value = u64::from_le_bytes(buffer);
    }

    for value in csr_state.mhpmevent.iter_mut() {
        reader.read_exact(&mut buffer)?;
        *value = u64::from_le_bytes(buffer);
    }

    Ok(())
}
