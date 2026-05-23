#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrapCause {
    InstructionAddressMisaligned,
    IllegalInstruction(u32),
    Breakpoint,
    LoadAddressMisaligned,
    StoreAddressMisaligned,
    EcallFromUMode,
    EcallFromSMode,
    EcallFromMMode,
}

impl TrapCause {
    pub fn mcause_code(&self) -> u64 {
        match self {
            Self::InstructionAddressMisaligned => 0,
            Self::IllegalInstruction(_) => 2,
            Self::Breakpoint => 3,
            Self::LoadAddressMisaligned => 4,
            Self::StoreAddressMisaligned => 6,
            Self::EcallFromUMode => 8,
            Self::EcallFromSMode => 9,
            Self::EcallFromMMode => 11,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepResult {
    Ok,
    Trap(TrapCause),
    Halt,
}
