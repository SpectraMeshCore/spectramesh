//! Restarting, and finding out why the chip last started.
//!
//! The chip records why it last reset, but a crash and a deliberate restart
//! look the same to it: both are software resets. So before resetting, this
//! module leaves a note in RTC memory, which survives a software reset.
//!
//! A panic restarts the node too (with esp-backtrace's `custom-halt`), so a
//! crashed node comes back instead of hanging until someone power-cycles it.

use core::ptr::{addr_of, addr_of_mut};

use esp_hal::delay::Delay;
use esp_hal::rtc_cntl::SocResetReason;
use spectramesh_core::ResetCause;

const RESTART: u32 = 0x5245_5354;
const PANIC: u32 = 0x5041_4e43;

/// Left uninitialized at boot, so it keeps what was written before a
/// software reset. After power-on it holds garbage, which matches no note.
#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut NOTE: u32 = 0;

fn leave_note(note: u32) {
    // SAFETY: a plain volatile write; only this module touches NOTE, and
    // never from two places at once (the next step is a reset).
    unsafe { addr_of_mut!(NOTE).write_volatile(note) };
}

/// Restarts the chip on purpose, such as to apply new settings.
pub fn restart() -> ! {
    leave_note(RESTART);
    esp_hal::system::software_reset()
}

/// Why the chip last started. Call once, early: it clears the note.
pub fn cause() -> ResetCause {
    // SAFETY: a plain volatile read and write, before any other task runs.
    let note = unsafe { addr_of!(NOTE).read_volatile() };
    leave_note(0);
    use SocResetReason::*;
    match esp_hal::system::reset_reason() {
        Some(ChipPowerOn) => ResetCause::PowerOn,
        Some(CoreSw | Cpu0Sw) if note == PANIC => ResetCause::Panic,
        Some(CoreSw | Cpu0Sw) => ResetCause::Restart,
        Some(CoreDeepSleep) => ResetCause::Wake,
        Some(SysBrownOut) => ResetCause::Brownout,
        Some(
            CoreMwdt0 | CoreMwdt1 | CoreRtcWdt | Cpu0Mwdt0 | Cpu0RtcWdt | SysRtcWdt | Cpu0Mwdt1
            | SysSuperWdt,
        ) => ResetCause::Watchdog,
        Some(CoreUsbUart | CoreUsbJtag | Cpu0JtagCpu) => ResetCause::External,
        _ => ResetCause::Unknown,
    }
}

/// Called by esp-backtrace after it prints a panic: restart rather than hang.
#[unsafe(no_mangle)]
fn custom_halt() -> ! {
    leave_note(PANIC);
    // A moment for the panic message to reach a connected host.
    Delay::new().delay_millis(500);
    esp_hal::system::software_reset()
}
