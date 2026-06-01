use std::cell::Cell;
use std::io::Write;

const RBR_THR: u8 = 0; // RBR = receiver buffer transmit holding register
const INTERRUPT_ENABLE_REGISTER: u8 = 1; // INTERRUPT_ENABLE_REGISTER = IER
const INTERRUPT_ID_FIFO_CONTROL_REGISTER: u8 = 2; // INTERRUPT_ID_FIFO_CONTROL_REGISTER = IIR
const LINE_CONTROL_REGISTER: u8 = 3; // LINE_CONTROL_REGISTER = LSR
const MODEM_CONTROL_REGISTER: u8 = 4;
const LINE_STATUS_REGISTER: u8 = 5; // LINE_STATUS_REGISTER = LSR
const SCRATCH_REGISTER: u8 = 7; // SCRATCH_REGISTER = SCR

const LSR_DATA_READY: u8 = 1 << 0;
const LSR_TX_EMPTY: u8 = 1 << 5;
const LSR_TX_IDLE: u8 = 1 << 6;

const IER_RX_READY: u8 = 1 << 0;
const IER_TX_EMPTY: u8 = 1 << 1;

const IIR_NO_INT: u8 = 0x01;
const IIR_RX_READY: u8 = 0x04;
const IIR_TX_EMPTY: u8 = 0x02;

pub struct Uart {
    interrupt_enable_register: Cell<u8>,
    line_control_register: Cell<u8>,
    modem_control_register: Cell<u8>,
    scratch_register: Cell<u8>,
    divisor_latch_low: Cell<u8>,
    divisor_latch_high: Cell<u8>,
    receive_buffer: Cell<std::collections::VecDeque<u8>>,
    pub irq_pending: Cell<bool>,
    pub capture_tx: Cell<bool>,
    pub transmit_buffer: Cell<Vec<u8>>,
}

impl Default for Uart {
    fn default() -> Self {
        Self::new()
    }
}

impl Uart {
    pub fn new() -> Self {
        Self {
            interrupt_enable_register: Cell::new(0),
            line_control_register: Cell::new(0),
            modem_control_register: Cell::new(0),
            scratch_register: Cell::new(0),
            divisor_latch_low: Cell::new(0),
            divisor_latch_high: Cell::new(0),
            receive_buffer: Cell::new(std::collections::VecDeque::new()),
            irq_pending: Cell::new(false),
            capture_tx: Cell::new(false),
            transmit_buffer: Cell::new(Vec::new()),
        }
    }

    pub fn serialize(&self, writer: &mut impl std::io::Write) -> std::io::Result<()> {
        writer.write_all(&[
            self.interrupt_enable_register.get(),
            self.line_control_register.get(),
            self.modem_control_register.get(),
            self.scratch_register.get(),
            self.divisor_latch_low.get(),
            self.divisor_latch_high.get(),
        ])?;

        let buffer = self.receive_buffer.take();
        writer.write_all(&(buffer.len() as u64).to_le_bytes())?;

        for byte in &buffer {
            writer.write_all(&[*byte])?;
        }

        self.receive_buffer.set(buffer);
        Ok(())
    }

    pub fn deserialize(&mut self, reader: &mut impl std::io::Read) -> std::io::Result<()> {
        let mut regs = [0u8; 6];

        reader.read_exact(&mut regs)?;

        self.interrupt_enable_register.set(regs[0]);
        self.line_control_register.set(regs[1]);
        self.modem_control_register.set(regs[2]);
        self.scratch_register.set(regs[3]);
        self.divisor_latch_low.set(regs[4]);
        self.divisor_latch_high.set(regs[5]);

        let mut length_buffer = [0u8; 8];

        reader.read_exact(&mut length_buffer)?;
        let len = u64::from_le_bytes(length_buffer) as usize;
        let mut buffer = std::collections::VecDeque::with_capacity(len);

        for _ in 0..len {
            let mut byte = [0u8; 1];

            reader.read_exact(&mut byte)?;
            buffer.push_back(byte[0]);
        }

        self.receive_buffer.set(buffer);
        self.update_irq();
        Ok(())
    }

    pub fn drain_tx(&self) -> Vec<u8> {
        self.transmit_buffer.replace(Vec::new())
    }

    pub fn tx_is_empty(&self) -> bool {
        let buffer = self.transmit_buffer.take();
        let empty = buffer.is_empty();
        self.transmit_buffer.set(buffer);

        empty
    }

    pub fn drain_rx(&self) {
        self.receive_buffer.set(std::collections::VecDeque::new());
    }

    pub fn push_rx(&self, byte: u8) {
        let mut buffer = self.receive_buffer.take();
        buffer.push_back(byte);

        self.receive_buffer.set(buffer);
        self.update_irq();
    }

    pub fn rx_pending(&self) -> bool {
        let buffer = self.receive_buffer.take();
        let pending = !buffer.is_empty();
        self.receive_buffer.set(buffer);
        pending
    }

    fn is_divisor_latch_mode(&self) -> bool {
        self.line_control_register.get() & 0x80 != 0
    }

    pub fn read_register(&self, offset: u8) -> u8 {
        match offset {
            RBR_THR => {
                if self.is_divisor_latch_mode() {
                    return self.divisor_latch_low.get();
                }

                let mut buffer = self.receive_buffer.take();
                let value = buffer.pop_front().unwrap_or(0);

                self.receive_buffer.set(buffer);
                self.update_irq();

                value
            }
            INTERRUPT_ENABLE_REGISTER => {
                if self.is_divisor_latch_mode() {
                    return self.divisor_latch_high.get();
                }

                self.interrupt_enable_register.get()
            }
            INTERRUPT_ID_FIFO_CONTROL_REGISTER => self.get_interrupt_id(),
            LINE_CONTROL_REGISTER => self.line_control_register.get(),
            MODEM_CONTROL_REGISTER => self.modem_control_register.get(),
            LINE_STATUS_REGISTER => self.get_line_status(),
            SCRATCH_REGISTER => self.scratch_register.get(),
            _ => 0,
        }
    }

    pub fn write_register(&self, offset: u8, value: u8) {
        match offset {
            RBR_THR => {
                if self.is_divisor_latch_mode() {
                    self.divisor_latch_low.set(value);
                    return;
                }

                if self.capture_tx.get() {
                    let mut buffer = self.transmit_buffer.take();
                    buffer.push(value);
                    self.transmit_buffer.set(buffer);
                } else {
                    let _ = std::io::stdout().write_all(&[value]);
                    let _ = std::io::stdout().flush();
                }

                self.update_irq();
            }
            INTERRUPT_ENABLE_REGISTER => {
                if self.is_divisor_latch_mode() {
                    self.divisor_latch_high.set(value);
                    return;
                }

                self.interrupt_enable_register.set(value);
                self.update_irq();
            }
            INTERRUPT_ID_FIFO_CONTROL_REGISTER => {}
            LINE_CONTROL_REGISTER => self.line_control_register.set(value),
            MODEM_CONTROL_REGISTER => self.modem_control_register.set(value),
            LINE_STATUS_REGISTER => {}
            SCRATCH_REGISTER => self.scratch_register.set(value),
            _ => {}
        }
    }

    fn get_line_status(&self) -> u8 {
        let buffer = self.receive_buffer.take();
        let has_data = !buffer.is_empty();
        self.receive_buffer.set(buffer);
        let mut lsr = LSR_TX_EMPTY | LSR_TX_IDLE;

        if has_data {
            lsr |= LSR_DATA_READY;
        }

        lsr
    }

    fn get_interrupt_id(&self) -> u8 {
        let buffer = self.receive_buffer.take();
        let has_data = !buffer.is_empty();
        self.receive_buffer.set(buffer);

        if self.interrupt_enable_register.get() & IER_RX_READY != 0 && has_data {
            return IIR_RX_READY;
        }

        if self.interrupt_enable_register.get() & IER_TX_EMPTY != 0 {
            return IIR_TX_EMPTY;
        }

        IIR_NO_INT
    }

    fn update_irq(&self) {
        self.irq_pending.set(self.get_interrupt_id() != IIR_NO_INT);
    }
}
