use std::io::{self, Read, Write};

use riscv_core::Hart;
use riscv_core::csr::PrivMode;

use crate::LOW_RAM_SIZE;
use crate::machine_bus::MachineBus;

const MAGIC: &[u8; 4] = b"TEMU";
const VERSION: u8 = 2;

pub fn save(bus: &MachineBus, hart: &Hart, w: &mut impl Write) -> io::Result<()> {
    w.write_all(MAGIC)?;
    w.write_all(&[VERSION])?;

    let ram_size = bus.ram_size();
    w.write_all(&ram_size.to_le_bytes())?;
    w.write_all(&bus.ram)?;
    w.write_all(&bus.low_ram)?;

    save_hart(hart, w)?;
    bus.clint.save(w)?;
    bus.plic.save(w)?;
    bus.uart.save(w)?;

    Ok(())
}

pub fn restore(bus: &mut MachineBus, hart: &mut Hart, r: &mut impl Read) -> io::Result<()> {
    let mut magic = [0u8; 4];
    r.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bad snapshot magic",
        ));
    }

    let mut ver = [0u8; 1];
    r.read_exact(&mut ver)?;
    if ver[0] != VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported snapshot version {}", ver[0]),
        ));
    }

    let mut b8 = [0u8; 8];
    r.read_exact(&mut b8)?;
    let ram_size = u64::from_le_bytes(b8);
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

    r.read_exact(&mut bus.ram)?;

    let mut low = vec![0u8; LOW_RAM_SIZE as usize];
    r.read_exact(&mut low)?;
    bus.low_ram = low;

    restore_hart(hart, r)?;
    bus.clint.restore(r)?;
    bus.plic.restore(r)?;
    bus.uart.restore(r)?;

    Ok(())
}

fn save_hart(hart: &Hart, w: &mut impl Write) -> io::Result<()> {
    for i in 0..32usize {
        w.write_all(&hart.regs.read(i).to_le_bytes())?;
    }

    w.write_all(&hart.regs.pc.to_le_bytes())?;
    for i in 0..32usize {
        w.write_all(&hart.regs.read_f(i).to_le_bytes())?;
    }

    let pm: u8 = match hart.priv_mode {
        PrivMode::U => 0,
        PrivMode::S => 1,
        PrivMode::M => 3,
    };
    w.write_all(&[pm])?;

    match hart.lr_addr {
        None => w.write_all(&[0u8])?,
        Some(a) => {
            w.write_all(&[1u8])?;
            w.write_all(&a.to_le_bytes())?;
        }
    }

    w.write_all(&hart.fetch_vpage.to_le_bytes())?;
    w.write_all(&hart.fetch_ppage.to_le_bytes())?;
    w.write_all(&hart.fetch_satp.to_le_bytes())?;

    save_csr(hart, w)
}

fn restore_hart(hart: &mut Hart, r: &mut impl Read) -> io::Result<()> {
    let mut b8 = [0u8; 8];

    for i in 0..32usize {
        r.read_exact(&mut b8)?;
        hart.regs.write(i, u64::from_le_bytes(b8));
    }

    r.read_exact(&mut b8)?;
    hart.regs.pc = u64::from_le_bytes(b8);
    for i in 0..32usize {
        r.read_exact(&mut b8)?;
        hart.regs.write_f(i, u64::from_le_bytes(b8));
    }

    let mut b1 = [0u8; 1];
    r.read_exact(&mut b1)?;
    hart.priv_mode = match b1[0] {
        0 => PrivMode::U,
        1 => PrivMode::S,
        _ => PrivMode::M,
    };

    r.read_exact(&mut b1)?;
    hart.lr_addr = if b1[0] == 0 {
        None
    } else {
        r.read_exact(&mut b8)?;
        Some(u64::from_le_bytes(b8))
    };

    r.read_exact(&mut b8)?;
    hart.fetch_vpage = u64::from_le_bytes(b8);
    r.read_exact(&mut b8)?;
    hart.fetch_ppage = u64::from_le_bytes(b8);
    r.read_exact(&mut b8)?;
    hart.fetch_satp = u64::from_le_bytes(b8);

    restore_csr(hart, r)
}

fn save_csr(hart: &Hart, w: &mut impl Write) -> io::Result<()> {
    let c = &hart.csr;

    macro_rules! wu64 {
        ($f:expr) => {
            w.write_all(&$f.to_le_bytes())?
        };
    }
    wu64!(c.mstatus);
    wu64!(c.misa);
    wu64!(c.medeleg);
    wu64!(c.mideleg);
    wu64!(c.mie);
    wu64!(c.mtvec);
    wu64!(c.mcounteren);
    wu64!(c.mcountinhibit);
    wu64!(c.mscratch);
    wu64!(c.mepc);
    wu64!(c.mcause);
    wu64!(c.mtval);
    wu64!(c.mip);
    wu64!(c.menvcfg);
    wu64!(c.stvec);
    wu64!(c.scounteren);
    wu64!(c.senvcfg);
    wu64!(c.sscratch);
    wu64!(c.sepc);
    wu64!(c.scause);
    wu64!(c.stval);
    wu64!(c.satp);
    wu64!(c.cycle);
    wu64!(c.instret);
    wu64!(c.time);
    wu64!(c.fcsr);
    wu64!(c.vtype);
    wu64!(c.vl);
    wu64!(c.vstart);
    wu64!(c.vcsr);

    for v in &c.pmpcfg {
        wu64!(v);
    }
    for v in &c.pmpaddr {
        wu64!(v);
    }
    for v in &c.mhpmevent {
        wu64!(v);
    }

    for vreg in hart.vregs.iter() {
        w.write_all(vreg)?;
    }
    Ok(())
}

fn restore_csr(hart: &mut Hart, r: &mut impl Read) -> io::Result<()> {
    let c = &mut hart.csr;
    let mut b = [0u8; 8];

    macro_rules! ru64 {
        ($f:expr) => {
            r.read_exact(&mut b)?;
            $f = u64::from_le_bytes(b);
        };
    }
    ru64!(c.mstatus);
    ru64!(c.misa);
    ru64!(c.medeleg);
    ru64!(c.mideleg);
    ru64!(c.mie);
    ru64!(c.mtvec);
    ru64!(c.mcounteren);
    ru64!(c.mcountinhibit);
    ru64!(c.mscratch);
    ru64!(c.mepc);
    ru64!(c.mcause);
    ru64!(c.mtval);
    ru64!(c.mip);
    ru64!(c.menvcfg);
    ru64!(c.stvec);
    ru64!(c.scounteren);
    ru64!(c.senvcfg);
    ru64!(c.sscratch);
    ru64!(c.sepc);
    ru64!(c.scause);
    ru64!(c.stval);
    ru64!(c.satp);
    ru64!(c.cycle);
    ru64!(c.instret);
    ru64!(c.time);
    ru64!(c.fcsr);
    ru64!(c.vtype);
    ru64!(c.vl);
    ru64!(c.vstart);
    ru64!(c.vcsr);

    for v in c.pmpcfg.iter_mut() {
        r.read_exact(&mut b)?;
        *v = u64::from_le_bytes(b);
    }

    for v in c.pmpaddr.iter_mut() {
        r.read_exact(&mut b)?;
        *v = u64::from_le_bytes(b);
    }

    for v in c.mhpmevent.iter_mut() {
        r.read_exact(&mut b)?;
        *v = u64::from_le_bytes(b);
    }

    for vreg in hart.vregs.iter_mut() {
        r.read_exact(vreg)?;
    }

    Ok(())
}
