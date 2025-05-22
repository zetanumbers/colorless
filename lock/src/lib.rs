use std::{
    fmt,
    ops::{Deref, DerefMut},
};

use colorless::{Stackify, inside_context};
use event_listener::{Event, Listener, listener};

#[derive(Default)]
pub struct Mutex<T: ?Sized> {
    inner: async_lock::Mutex<T>,
}

pub struct MutexGuard<'a, T: ?Sized>(async_lock::MutexGuard<'a, T>);

impl<'a, T: ?Sized> Deref for MutexGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, T: ?Sized> DerefMut for MutexGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> Mutex<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: async_lock::Mutex::new(data),
        }
    }

    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> Mutex<T> {
    pub fn lock(&self) -> MutexGuard<'_, T> {
        MutexGuard(
            self.inner
                .lock()
                .await_()
                .unwrap_or_else(|_| self.inner.lock_blocking()),
        )
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.inner.try_lock().map(MutexGuard)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Mutex<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.try_lock() {
            Some(guard) => f.debug_struct("Mutex").field("data", &&*guard).finish(),
            None => {
                struct LockedPlaceholder;
                impl fmt::Debug for LockedPlaceholder {
                    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("<locked>")
                    }
                }

                f.debug_struct("Mutex")
                    .field("data", &LockedPlaceholder)
                    .finish()
            }
        }
    }
}

#[derive(Default)]
pub struct Condvar(Event);

impl Condvar {
    pub const fn new() -> Self {
        Condvar(Event::new())
    }

    pub fn notify_one(&self) {
        self.0.notify(1);
    }

    pub fn notify_all(&self) {
        self.0.notify(usize::MAX);
    }

    pub fn wait<'a, T: ?Sized>(&self, mutex_guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        listener!(&self.0 => listener);
        let mutex = async_lock::MutexGuard::source(&mutex_guard.0);
        drop(mutex_guard);
        if inside_context() {
            listener.await_().unwrap();
            MutexGuard(mutex.lock().await_().unwrap())
        } else {
            listener.wait();
            MutexGuard(mutex.lock_blocking())
        }
    }
}

impl fmt::Debug for Condvar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad("Condvar { .. }")
    }
}

#[derive(Default)]
pub struct RwLock<T: ?Sized> {
    inner: async_lock::RwLock<T>,
}

impl<T> RwLock<T> {
    pub const fn new(data: T) -> Self {
        Self {
            inner: async_lock::RwLock::new(data),
        }
    }

    pub fn into_inner(self) -> T {
        self.inner.into_inner()
    }
}

impl<T: ?Sized> RwLock<T> {
    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        RwLockReadGuard(
            self.inner
                .read()
                .await_()
                .unwrap_or_else(|_| self.inner.read_blocking()),
        )
    }

    pub fn try_read(&self) -> Option<RwLockReadGuard<'_, T>> {
        self.inner.try_read().map(RwLockReadGuard)
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        RwLockWriteGuard(
            self.inner
                .write()
                .await_()
                .unwrap_or_else(|_| self.inner.write_blocking()),
        )
    }

    pub fn try_write(&self) -> Option<RwLockWriteGuard<'_, T>> {
        self.inner.try_write().map(RwLockWriteGuard)
    }

    pub fn get_mut(&mut self) -> &mut T {
        self.inner.get_mut()
    }
}

pub struct RwLockReadGuard<'a, T: ?Sized>(async_lock::RwLockReadGuard<'a, T>);

impl<'a, T: ?Sized> Deref for RwLockReadGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

pub struct RwLockWriteGuard<'a, T: ?Sized>(async_lock::RwLockWriteGuard<'a, T>);

impl<'a, T: ?Sized> Deref for RwLockWriteGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a, T: ?Sized> DerefMut for RwLockWriteGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for RwLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("RwLock");
        match self.try_read() {
            Some(guard) => d.field("data", &&*guard),
            None => {
                // Additional format_args! here is to remove quotes around <locked> in debug output.
                d.field("data", &format_args!("<locked>"))
            }
        };
        d.finish()
    }
}
