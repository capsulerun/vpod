use std::collections::HashMap;

use riscv_core::{GuestMemory, SystemBus};

const IDENTITY_SCAN_BYTES: u64 = 8192;
const PARENT_SCAN_BYTES: u64 = 256;
const REQUIRED_SUPPORT: usize = 2;
const MAX_CANDIDATES: usize = 512;
const PID_MAX_LIMIT: u32 = 4 * 1024 * 1024;

pub struct TaskLayout {
    pub thread_id: u64,
    pub process_id: u64,
    pub real_parent: Option<u64>,
}

#[derive(Default)]
pub struct Identities {
    layout: Option<TaskLayout>,
    identity_support: HashMap<u64, Vec<u32>>,
    parent_support: HashMap<u64, Vec<u64>>,
}

impl Identities {
    pub fn calibrated(&self) -> bool {
        self.layout.is_some()
    }

    pub fn knows_parents(&self) -> bool {
        matches!(&self.layout, Some(layout) if layout.real_parent.is_some())
    }

    pub fn observe_identity<B: SystemBus>(
        &mut self,
        task: u64,
        value: u32,
        bus: &mut B,
        satp: u64,
    ) {
        if self.layout.is_some() || value == 0 || value > PID_MAX_LIMIT {
            return;
        }

        let mut memory = GuestMemory::new(satp);
        let mut previous = None;
        let mut found = Vec::new();

        for offset in (0..IDENTITY_SCAN_BYTES).step_by(4) {
            let Some(word) = memory.u32(bus, task.wrapping_add(offset)) else {
                break;
            };
            if word == value && previous == Some(value) {
                found.push(offset - 4);
            }
            previous = Some(word);
        }

        for offset in found {
            support(&mut self.identity_support, offset, value);
        }

        if let Some(offset) = settled(&self.identity_support) {
            self.layout = Some(TaskLayout {
                thread_id: offset,
                process_id: offset + 4,
                real_parent: None,
            });
            self.identity_support = HashMap::new();
        }
    }

    pub fn observe_parent<B: SystemBus>(
        &mut self,
        child_task: u64,
        parent_task: u64,
        bus: &mut B,
        satp: u64,
    ) {
        let Some(layout) = &self.layout else {
            return;
        };
        if layout.real_parent.is_some() || !is_kernel_pointer(parent_task) {
            return;
        }

        let start = (layout.process_id + 4).next_multiple_of(8);
        let mut memory = GuestMemory::new(satp);
        let mut found = Vec::new();

        for offset in (start..start + PARENT_SCAN_BYTES).step_by(8) {
            let Some(word) = memory.u64(bus, child_task.wrapping_add(offset)) else {
                break;
            };
            if word == parent_task {
                found.push(offset);
            }
        }

        for offset in found {
            support(&mut self.parent_support, offset, parent_task);
        }

        if let Some(offset) = lowest_settled(&self.parent_support)
            && let Some(layout) = &mut self.layout
        {
            layout.real_parent = Some(offset);
            self.parent_support = HashMap::new();
        }
    }

    pub fn process_of<B: SystemBus>(&self, task: u64, bus: &mut B, satp: u64) -> Option<u32> {
        let layout = self.layout.as_ref()?;
        let value = GuestMemory::new(satp).u32(bus, task.wrapping_add(layout.process_id))?;
        (value > 0 && value <= PID_MAX_LIMIT).then_some(value)
    }

    pub fn thread_of<B: SystemBus>(&self, task: u64, bus: &mut B, satp: u64) -> Option<u32> {
        let layout = self.layout.as_ref()?;
        let value = GuestMemory::new(satp).u32(bus, task.wrapping_add(layout.thread_id))?;
        (value > 0 && value <= PID_MAX_LIMIT).then_some(value)
    }

    pub fn parent_task_of<B: SystemBus>(&self, task: u64, bus: &mut B, satp: u64) -> Option<u64> {
        let layout = self.layout.as_ref()?;
        let pointer = GuestMemory::new(satp).u64(bus, task.wrapping_add(layout.real_parent?))?;
        is_kernel_pointer(pointer).then_some(pointer)
    }
}

fn support<T: PartialEq>(table: &mut HashMap<u64, Vec<T>>, offset: u64, value: T) {
    if table.len() >= MAX_CANDIDATES && !table.contains_key(&offset) {
        return;
    }
    let seen = table.entry(offset).or_default();
    if !seen.contains(&value) {
        seen.push(value);
    }
}

fn settled<T>(table: &HashMap<u64, Vec<T>>) -> Option<u64> {
    let best = table.values().map(Vec::len).max()?;
    if best < REQUIRED_SUPPORT {
        return None;
    }
    let mut winners = table
        .iter()
        .filter(|(_, values)| values.len() == best)
        .map(|(offset, _)| *offset);

    let first = winners.next()?;
    winners.next().is_none().then_some(first)
}

fn lowest_settled<T>(table: &HashMap<u64, Vec<T>>) -> Option<u64> {
    let best = table.values().map(Vec::len).max()?;
    (best >= REQUIRED_SUPPORT)
        .then(|| {
            table
                .iter()
                .filter(|(_, values)| values.len() == best)
                .map(|(offset, _)| *offset)
                .min()
        })
        .flatten()
}

fn is_kernel_pointer(pointer: u64) -> bool {
    pointer >> 56 == 0xff && pointer.is_multiple_of(8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riscv_core::FlatMemory;

    const PID_OFFSET: u64 = 1296;
    const REAL_PARENT_OFFSET: u64 = 1312;

    struct Guest {
        memory: FlatMemory,
    }

    impl Guest {
        fn new() -> Self {
            Self {
                memory: FlatMemory::new(1024 * 1024),
            }
        }

        fn task(&mut self, address: u64, thread_id: u32, process_id: u32, parent: u64) {
            self.memory
                .load_at((address + PID_OFFSET) as usize, &thread_id.to_le_bytes());
            self.memory.load_at(
                (address + PID_OFFSET + 4) as usize,
                &process_id.to_le_bytes(),
            );
            self.memory.load_at(
                (address + REAL_PARENT_OFFSET) as usize,
                &parent.to_le_bytes(),
            );
        }
    }

    #[test]
    fn two_tasks_naming_themselves_pin_the_pid_and_tgid_offsets() {
        let mut guest = Guest::new();
        let mut identities = Identities::default();
        guest.task(0x10000, 556, 556, 0);
        guest.task(0x20000, 557, 557, 0);

        identities.observe_identity(0x10000, 556, &mut guest.memory, 0);
        assert!(!identities.calibrated(), "one task cannot settle an offset");

        identities.observe_identity(0x20000, 557, &mut guest.memory, 0);

        assert!(identities.calibrated());
        assert_eq!(
            identities.process_of(0x10000, &mut guest.memory, 0),
            Some(556)
        );
        assert_eq!(
            identities.process_of(0x20000, &mut guest.memory, 0),
            Some(557)
        );
    }

    #[test]
    fn a_thread_naming_itself_does_not_settle_anything_on_its_own() {
        let mut guest = Guest::new();
        let mut identities = Identities::default();
        guest.task(0x10000, 566, 565, 0);
        guest.task(0x20000, 570, 569, 0);

        identities.observe_identity(0x10000, 566, &mut guest.memory, 0);
        identities.observe_identity(0x20000, 570, &mut guest.memory, 0);

        assert!(!identities.calibrated());
    }

    #[test]
    fn a_process_that_is_a_thread_group_reports_the_group_not_the_thread() {
        let mut guest = Guest::new();
        let mut identities = Identities::default();
        guest.task(0x10000, 556, 556, 0);
        guest.task(0x20000, 557, 557, 0);
        identities.observe_identity(0x10000, 556, &mut guest.memory, 0);
        identities.observe_identity(0x20000, 557, &mut guest.memory, 0);

        guest.task(0x30000, 566, 565, 0);

        assert_eq!(
            identities.process_of(0x30000, &mut guest.memory, 0),
            Some(565)
        );
        assert_eq!(
            identities.thread_of(0x30000, &mut guest.memory, 0),
            Some(566)
        );
    }

    #[test]
    fn two_children_pointing_at_their_parents_pin_the_parent_offset() {
        let mut guest = Guest::new();
        let mut identities = Identities::default();
        let first_parent = 0xffff_ffd6_0000_1000;
        let second_parent = 0xffff_ffd6_0000_2000;

        guest.task(0x10000, 556, 556, first_parent);
        guest.task(0x20000, 557, 557, second_parent);
        identities.observe_identity(0x10000, 556, &mut guest.memory, 0);
        identities.observe_identity(0x20000, 557, &mut guest.memory, 0);

        identities.observe_parent(0x10000, first_parent, &mut guest.memory, 0);
        assert!(!identities.knows_parents());

        identities.observe_parent(0x20000, second_parent, &mut guest.memory, 0);

        assert!(identities.knows_parents());
        assert_eq!(
            identities.parent_task_of(0x10000, &mut guest.memory, 0),
            Some(first_parent)
        );
    }

    #[test]
    fn an_implausible_process_id_is_not_reported() {
        let mut guest = Guest::new();
        let mut identities = Identities::default();
        guest.task(0x10000, 556, 556, 0);
        guest.task(0x20000, 557, 557, 0);
        identities.observe_identity(0x10000, 556, &mut guest.memory, 0);
        identities.observe_identity(0x20000, 557, &mut guest.memory, 0);

        guest.task(0x30000, 0, 0, 0);
        assert_eq!(identities.process_of(0x30000, &mut guest.memory, 0), None);

        guest.task(0x40000, 9_000_000, 9_000_000, 0);
        assert_eq!(identities.process_of(0x40000, &mut guest.memory, 0), None);
    }

    #[test]
    fn a_task_whose_memory_holds_no_matching_pair_never_calibrates() {
        let mut guest = Guest::new();
        let mut identities = Identities::default();

        identities.observe_identity(0x10000, 556, &mut guest.memory, 0);
        identities.observe_identity(0x20000, 557, &mut guest.memory, 0);

        assert!(!identities.calibrated());
    }
}
