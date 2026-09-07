//! A tiny spin lock for single-address-space kernel data structures.
//!
//! This lock only guarantees mutual exclusion.  Code that can be preempted by
//! an interrupt handler that *also* takes the lock must disable interrupts
//! around the critical section (see [`crate::arch::InterruptGuard`]); otherwise
//! the owner of the lock may be switched away and the ISR spins forever.

use core::cell::UnsafeCell;
use core::hint;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

// Safety: access is serialized by `locked`; `T` is only touched while held.
unsafe impl<T: Send> Sync for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        while self.locked.swap(true, Ordering::Acquire) {
            hint::spin_loop();
        }
        SpinLockGuard { lock: self }
    }

    /// Call `f` with the lock held.  Convenience for `&mut self`-style use on
    /// a static.
    #[inline]
    pub fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let mut guard = self.lock();
        f(&mut *guard)
    }
}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
    }
}
