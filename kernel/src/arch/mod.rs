pub mod x86;

/// Arch-neutral re-exports used by the arch-agnostic kernel core.
///
/// When a second architecture (x86-64, RISC-V, ...) is added, `arch/mod.rs`
/// will `cfg`-select its own backend for each of these names.
pub use x86::intrctl::InterruptGuard;
