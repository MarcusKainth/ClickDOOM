//! The device window: five registers, and the inert byte-backed alternative.
//!
//! Word access only. An access that is not word-width, or whose offset is not
//! one of the five registers, reads 0 and is ignored on write. There is no
//! byte-addressable scratch here, because no conforming ROM needs one and
//! serving it would cost the SQL engine node budget on every retired
//! instruction rather than only the ones that touch a device.
//!
//! Nothing in this module reads a host clock or any source of randomness.
//! `TICKS_MS` is retired instructions divided by a constant, which is what
//! makes a run reproducible and speed-independent.

use std::collections::VecDeque;

use clickdoom_spec::map::mmio;

/// A frame the program announced as complete.
///
/// The two counts differ by one and both are needed. The device sees the
/// store while it executes, before the instruction retires, so `commit_icount`
/// is the count before it. `retired_icount` is the count after, which is the
/// convention every checkpoint uses.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct FrameCommit {
    pub frame_no: u32,
    pub commit_icount: u64,
}

impl FrameCommit {
    pub const fn retired_icount(&self) -> u64 {
        self.commit_icount + 1
    }
}

/// A write to the exit register. Not a fault: the program stopping on purpose.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct MmioExit {
    pub code: u32,
}

/// A key event waiting to be popped.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub struct KeyEvent {
    pub pressed: bool,
    pub doomkey: u8,
}

/// The five registers.
#[derive(Clone, Debug)]
pub struct Registers {
    ipms: u32,
    pub key_queue: VecDeque<KeyEvent>,
    pub console: Vec<u8>,
    pub frame_commits: Vec<FrameCommit>,
}

impl Registers {
    /// `ipms` is instructions per emulated millisecond and must not be zero,
    /// because the tick register divides by it.
    pub fn new(ipms: u32) -> Self {
        assert!(
            ipms != 0,
            "ipms is 0, which would divide by zero in TICKS_MS"
        );
        Self {
            ipms,
            key_queue: VecDeque::new(),
            console: Vec::new(),
            frame_commits: Vec::new(),
        }
    }

    pub const fn ipms(&self) -> u32 {
        self.ipms
    }

    /// Queues one key event. Call order is pop order.
    pub fn push_key(&mut self, pressed: bool, doomkey: u8) {
        self.key_queue.push_back(KeyEvent { pressed, doomkey });
    }

    fn read(&mut self, offset: u32, width: u32, icount: u64) -> u32 {
        if width != 4 {
            return 0;
        }
        match offset {
            mmio::TICKS_MS => (icount / self.ipms as u64) as u32,
            mmio::KEYQ => match self.key_queue.pop_front() {
                Some(event) => clickdoom_spec::map::key_event(event.pressed, event.doomkey),
                None => 0,
            },
            _ => 0,
        }
    }

    fn write(&mut self, offset: u32, width: u32, value: u32, icount: u64) -> Result<(), MmioExit> {
        if width != 4 {
            return Ok(());
        }
        match offset {
            mmio::EXIT => return Err(MmioExit { code: value }),
            mmio::PUTCHAR => self.console.push(value as u8),
            mmio::FRAME_COMMIT => self.frame_commits.push(FrameCommit {
                frame_no: value,
                commit_icount: icount,
            }),
            _ => {}
        }
        Ok(())
    }
}

/// Plain byte storage over the device window, with no register behaviour.
///
/// The riscv-tests fixtures run against this. They have no device model to
/// talk to, and a store that lands here must behave like memory rather than
/// stopping the machine.
#[derive(Clone, Debug)]
pub struct ByteWindow {
    bytes: Box<[u8]>,
}

impl ByteWindow {
    pub fn new(size: u32) -> Self {
        Self {
            bytes: vec![0; size as usize].into_boxed_slice(),
        }
    }

    fn read(&self, offset: u32, width: u32) -> u32 {
        let at = offset as usize;
        let mut value = 0u32;
        for i in 0..width as usize {
            value |= (self.bytes[at + i] as u32) << (8 * i);
        }
        value
    }

    fn write(&mut self, offset: u32, width: u32, value: u32) {
        let at = offset as usize;
        for i in 0..width as usize {
            self.bytes[at + i] = (value >> (8 * i)) as u8;
        }
    }
}

/// Which device model the window presents.
#[derive(Clone, Debug)]
pub enum Devices {
    Registers(Registers),
    Bytes(ByteWindow),
}

impl Devices {
    pub fn registers(ipms: u32) -> Self {
        Devices::Registers(Registers::new(ipms))
    }

    pub fn bytes(size: u32) -> Self {
        Devices::Bytes(ByteWindow::new(size))
    }

    pub fn read(&mut self, offset: u32, width: u32, icount: u64) -> u32 {
        match self {
            Devices::Registers(r) => r.read(offset, width, icount),
            Devices::Bytes(b) => b.read(offset, width),
        }
    }

    pub fn write(
        &mut self,
        offset: u32,
        width: u32,
        value: u32,
        icount: u64,
    ) -> Result<(), MmioExit> {
        match self {
            Devices::Registers(r) => r.write(offset, width, value, icount),
            Devices::Bytes(b) => {
                b.write(offset, width, value);
                Ok(())
            }
        }
    }

    /// The registers, when this window has them.
    pub fn registers_ref(&self) -> Option<&Registers> {
        match self {
            Devices::Registers(r) => Some(r),
            Devices::Bytes(_) => None,
        }
    }

    pub fn registers_mut(&mut self) -> Option<&mut Registers> {
        match self {
            Devices::Registers(r) => Some(r),
            Devices::Bytes(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clickdoom_spec::IPMS_DEFAULT;

    fn regs() -> Devices {
        Devices::registers(IPMS_DEFAULT)
    }

    #[test]
    fn ticks_come_from_retired_instructions_and_never_a_clock() {
        let mut d = Devices::registers(10);
        assert_eq!(d.read(mmio::TICKS_MS, 4, 0), 0);
        assert_eq!(d.read(mmio::TICKS_MS, 4, 9), 0);
        assert_eq!(d.read(mmio::TICKS_MS, 4, 25), 2);
        // The same count gives the same answer, whatever else has happened.
        assert_eq!(d.read(mmio::TICKS_MS, 4, 25), 2);
    }

    #[test]
    fn the_key_queue_pops_one_event_in_push_order() {
        let mut d = regs();
        let r = d.registers_mut().unwrap();
        r.push_key(true, 0x41);
        r.push_key(false, 0x41);
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0x141);
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0x041);
    }

    #[test]
    fn an_empty_key_queue_reads_zero_and_pops_nothing() {
        let mut d = regs();
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0);
        d.registers_mut().unwrap().push_key(true, 7);
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0x107);
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0);
    }

    #[test]
    fn a_key_event_survives_a_doomkey_with_the_high_bit_set() {
        let mut d = regs();
        d.registers_mut().unwrap().push_key(true, 0xFF);
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0x1FF);
    }

    #[test]
    fn exit_carries_the_written_value() {
        let mut d = regs();
        assert_eq!(
            d.write(mmio::EXIT, 4, 0xFFFF_FFFF, 0),
            Err(MmioExit { code: 0xFFFF_FFFF })
        );
    }

    #[test]
    fn putchar_keeps_only_the_low_byte() {
        let mut d = regs();
        d.write(mmio::PUTCHAR, 4, 0x141, 0).unwrap();
        assert_eq!(d.registers_ref().unwrap().console, b"A");
    }

    #[test]
    fn a_frame_commit_records_the_count_before_the_store_retires() {
        let mut d = regs();
        d.write(mmio::FRAME_COMMIT, 4, 42, 100).unwrap();
        let commit = d.registers_ref().unwrap().frame_commits[0];
        assert_eq!(commit.frame_no, 42);
        assert_eq!(commit.commit_icount, 100);
        assert_eq!(commit.retired_icount(), 101);
    }

    #[test]
    fn an_offset_that_is_not_a_register_reads_zero_and_ignores_a_write() {
        let mut d = regs();
        d.write(0x20, 4, 0xDEAD_BEEF, 0).unwrap();
        assert_eq!(d.read(0x20, 4, 0), 0);
        assert!(d.registers_ref().unwrap().console.is_empty());
        assert!(d.registers_ref().unwrap().frame_commits.is_empty());
    }

    #[test]
    fn a_narrow_access_to_a_real_register_reads_zero_and_ignores_a_write() {
        let mut d = Devices::registers(10);
        assert_eq!(d.read(mmio::TICKS_MS, 1, 25), 0);
        assert_eq!(d.read(mmio::KEYQ, 2, 0), 0);
        // A byte write to the exit register does not stop the machine.
        assert_eq!(d.write(mmio::EXIT, 1, 1, 0), Ok(()));
        d.write(mmio::PUTCHAR, 2, b'A' as u32, 0).unwrap();
        assert!(d.registers_ref().unwrap().console.is_empty());
    }

    #[test]
    fn a_narrow_key_read_does_not_pop() {
        let mut d = regs();
        d.registers_mut().unwrap().push_key(true, 9);
        assert_eq!(d.read(mmio::KEYQ, 1, 0), 0);
        assert_eq!(d.read(mmio::KEYQ, 4, 0), 0x109);
    }

    #[test]
    fn the_inert_window_stores_bytes_and_has_no_register_behaviour() {
        let mut d = Devices::bytes(4096);
        assert_eq!(d.read(mmio::TICKS_MS, 4, 1_000_000), 0);
        // The exit register is ordinary memory here.
        assert_eq!(d.write(mmio::EXIT, 4, 7, 0), Ok(()));
        assert_eq!(d.read(mmio::EXIT, 4, 0), 7);
        d.write(0x20, 1, 0xAB, 0).unwrap();
        assert_eq!(d.read(0x20, 1, 0), 0xAB);
        assert_eq!(d.read(0x20, 4, 0), 0xAB);
        assert!(d.registers_ref().is_none());
    }

    #[test]
    #[should_panic(expected = "ipms is 0")]
    fn an_ipms_of_zero_is_refused_rather_than_dividing_by_it() {
        Devices::registers(0);
    }
}
