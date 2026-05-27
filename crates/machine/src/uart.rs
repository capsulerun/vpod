use std::cell::Cell;
use std::io::Write;

const RBR_THR: u8 = 0;
const IER: u8 = 1;
const IIR_FCR: u8 = 2;
const LCR: u8 = 3;
const MCR: u8 = 4;
const LSR: u8 = 5;
const SCR: u8 = 7;

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_TX_EMPTY: u8 = 1 << 5;
const LSR_TX_IDLE: u8 = 1 << 6;

const IER_RX_READY: u8 = 1 << 0;
const IER_TX_EMPTY: u8 = 1 << 1;

const IIR_NO_INT: u8 = 0x01;
const IIR_RX_READY: u8 = 0x04;
const IIR_TX_EMPTY: u8 = 0x02;

pub struct Uart {
    ier: Cell<u8>,
    lcr: Cell<u8>,
    mcr: Cell<u8>,
    scr: Cell<u8>,
    dll: Cell<u8>,
    dlh: Cell<u8>,
    rx_buf: Cell<std::collections::VecDeque<u8>>,
    pub irq_pending: Cell<bool>,
    pub capture_tx: Cell<bool>,
    pub tx_buf: Cell<Vec<u8>>,
}

impl Default for Uart {
    fn default() -> Self {
        Self::new()
    }
}

impl Uart {
    pub fn new() -> Self {
        Self {
            ier: Cell::new(0),
            lcr: Cell::new(0),
            mcr: Cell::new(0),
            scr: Cell::new(0),
            dll: Cell::new(0),
            dlh: Cell::new(0),
            rx_buf: Cell::new(std::collections::VecDeque::new()),
            irq_pending: Cell::new(false),
            capture_tx: Cell::new(false),
            tx_buf: Cell::new(Vec::new()),
        }
    }

    pub fn save(&self, w: &mut impl std::io::Write) -> std::io::Result<()> {
        w.write_all(&[
            self.ier.get(),
            self.lcr.get(),
            self.mcr.get(),
            self.scr.get(),
            self.dll.get(),
            self.dlh.get(),
        ])?;

        let buf = self.rx_buf.take();
        w.write_all(&(buf.len() as u64).to_le_bytes())?;

        for b in &buf {
            w.write_all(&[*b])?;
        }

        self.rx_buf.set(buf);
        Ok(())
    }

    pub fn restore(&mut self, r: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut regs = [0u8; 6];

        r.read_exact(&mut regs)?;

        self.ier.set(regs[0]);
        self.lcr.set(regs[1]);
        self.mcr.set(regs[2]);
        self.scr.set(regs[3]);
        self.dll.set(regs[4]);
        self.dlh.set(regs[5]);

        let mut b8 = [0u8; 8];

        r.read_exact(&mut b8)?;
        let len = u64::from_le_bytes(b8) as usize;
        let mut buf = std::collections::VecDeque::with_capacity(len);

        for _ in 0..len {
            let mut b = [0u8; 1];
            r.read_exact(&mut b)?;
            buf.push_back(b[0]);
        }

        self.rx_buf.set(buf);
        self.update_irq();
        Ok(())
    }

    pub fn drain_tx(&self) -> Vec<u8> {
        self.tx_buf.replace(Vec::new())
    }

    pub fn drain_rx(&self) {
        self.rx_buf.set(std::collections::VecDeque::new());
    }

    pub fn push_rx(&self, byte: u8) {
        let mut buf = self.rx_buf.take();
        buf.push_back(byte);

        self.rx_buf.set(buf);
        self.update_irq();
    }

    fn dlab(&self) -> bool {
        self.lcr.get() & 0x80 != 0
    }

    pub fn read(&self, offset: u8) -> u8 {
        match offset {
            RBR_THR => {
                if self.dlab() {
                    return self.dll.get();
                }
                let mut buf = self.rx_buf.take();
                let val = buf.pop_front().unwrap_or(0);

                self.rx_buf.set(buf);
                self.update_irq();
                val
            }
            IER => {
                if self.dlab() {
                    return self.dlh.get();
                }
                self.ier.get()
            }
            IIR_FCR => self.iir(),
            LCR => self.lcr.get(),
            MCR => self.mcr.get(),
            LSR => self.lsr(),
            SCR => self.scr.get(),
            _ => 0,
        }
    }

    pub fn write(&self, offset: u8, val: u8) {
        match offset {
            RBR_THR => {
                if self.dlab() {
                    self.dll.set(val);
                    return;
                }
                if self.capture_tx.get() {
                    let mut buf = self.tx_buf.take();
                    buf.push(val);
                    self.tx_buf.set(buf);
                } else {

                    let _ = std::io::stdout().write_all(&[val]);
                    let _ = std::io::stdout().flush();
                }
                self.update_irq();
            }
            IER => {
                if self.dlab() {
                    self.dlh.set(val);
                    return;
                }
                self.ier.set(val);
                self.update_irq();
            }
            IIR_FCR => {}
            LCR => self.lcr.set(val),
            MCR => self.mcr.set(val),
            LSR => {}
            SCR => self.scr.set(val),
            _ => {}
        }
    }

    fn lsr(&self) -> u8 {
        let buf = self.rx_buf.take();
        let has_data = !buf.is_empty();
        self.rx_buf.set(buf);
        let mut lsr = LSR_TX_EMPTY | LSR_TX_IDLE;

        if has_data {
            lsr |= LSR_DATA_READY;
        }

        lsr
    }

    fn iir(&self) -> u8 {
        let buf = self.rx_buf.take();
        let has_data = !buf.is_empty();
        self.rx_buf.set(buf);

        if self.ier.get() & IER_RX_READY != 0 && has_data {
            return IIR_RX_READY;
        }

        if self.ier.get() & IER_TX_EMPTY != 0 {
            return IIR_TX_EMPTY;
        }

        IIR_NO_INT
    }

    fn update_irq(&self) {
        self.irq_pending.set(self.iir() != IIR_NO_INT);
    }
}
