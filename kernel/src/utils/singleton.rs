use core::{
    cell::UnsafeCell,
    hint::spin_loop,
    mem::MaybeUninit,
    ops::Deref,
    sync::atomic::{AtomicU8, Ordering},
};

const SINGLE_UNINIT: u8 = 0;
const SINGLE_INITING: u8 = 1;
const SINGLE_INITED: u8 = 2;

pub struct Singleton<T: Default> {
    pub inited: AtomicU8,
    data: UnsafeCell<MaybeUninit<T>>,
}

unsafe impl<T: Default> Sync for Singleton<T> {}

impl<T: Default> Singleton<T> {
    pub const UNINIT: Singleton<T> = Self {
        inited: AtomicU8::new(SINGLE_UNINIT),
        data: UnsafeCell::new(MaybeUninit::uninit()),
    };

    fn init(&self) {
        loop {
            let stat = self.inited.compare_exchange(
                SINGLE_UNINIT,
                SINGLE_INITING,
                Ordering::Acquire,
                Ordering::Acquire,
            );
            match stat {
                Ok(_) => {}
                Err(SINGLE_INITED) => {
                    break;
                }
                Err(SINGLE_UNINIT) => {
                    continue;
                }
                Err(SINGLE_INITING) => {
                    while self.inited.load(Ordering::Acquire) == SINGLE_INITING {
                        spin_loop()
                    }
                    continue;
                }
                Err(_) => {}
            }

            unsafe { (*self.data.get()).as_mut_ptr().write(T::default()) };
            self.inited.store(SINGLE_INITED, Ordering::Release);

            break;
        }
    }

    fn get(&self) -> &T {
        self.init();
        unsafe { &*(*self.data.get()).as_ptr() }
    }

    pub fn get_mut(&self) -> &mut T {
        self.init();
        unsafe { &mut *(*self.data.get()).as_mut_ptr() }
    }
}

impl<T: Default> Deref for Singleton<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T: Default> Default for Singleton<T> {
    fn default() -> Self {
        Self {
            inited: AtomicU8::new(SINGLE_UNINIT),
            data: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
}

impl<T: Default> Drop for Singleton<T> {
    fn drop(&mut self) {
        if self.inited.load(Ordering::Acquire) != SINGLE_INITED {
            unsafe { (*self.data.get()).assume_init_drop() };
        }
    }
}
