use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::LazyLock;

use crate::repl;
use machine::machine_bus::MachineBus;
use riscv_core::Hart;

pub struct Session {
    pub bus: MachineBus,
    pub hart: Hart,
    pub prompt: Vec<u8>,
}

pub struct SessionManager {
    sessions: RefCell<HashMap<u64, Session>>,
    next_id: Cell<u64>,
}

// for non blocking aspect
unsafe impl Sync for SessionManager {}

pub static SESSION_MANAGER: LazyLock<SessionManager> = LazyLock::new(|| SessionManager {
    sessions: RefCell::new(HashMap::new()),
    next_id: Cell::new(1),
});

impl SessionManager {
    pub fn start_session(
        &self,
        snapshot_path: String,
        command: String,
        prompt: String,
    ) -> Result<u64, String> {
        let (mut bus, mut hart) = crate::vm::load(crate::vm::VmConfig {
            snapshot: snapshot_path.as_ref(),
            disk: None,
            capture_tx: false,
        })?;

        let prompt_bytes = prompt.into_bytes();

        for byte in command.bytes() {
            bus.uart.push_rx(byte);
        }

        bus.uart.push_rx(b'\n');
        repl::wait_for_prompt(&mut bus, &mut hart, &prompt_bytes);

        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.sessions.borrow_mut().insert(
            id,
            Session {
                bus,
                hart,
                prompt: prompt_bytes,
            },
        );

        Ok(id)
    }

    pub fn exec_code(&self, handle: u64, code: String) -> Result<String, String> {
        let mut sessions = self.sessions.borrow_mut();
        let session = sessions
            .get_mut(&handle)
            .ok_or_else(|| format!("invalid session handle: {handle}"))?;

        for byte in code.bytes() {
            session.bus.uart.push_rx(byte);
        }
        session.bus.uart.push_rx(b'\n');

        Ok(repl::capture_output_until_prompt(
            &mut session.bus,
            &mut session.hart,
            &session.prompt,
        ))
    }

    pub fn close_session(&self, handle: u64) {
        self.sessions.borrow_mut().remove(&handle);
    }
}
