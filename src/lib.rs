//! `accounting-allocator` is a global memory allocator wrapper which counts allocated and deallocated bytes.
//!
//! # Usage
//!
//! ```
//! use accounting_allocator::{AccountingAlloc, AllocCounts};
//!
//! #[global_allocator]
//! static GLOBAL_ALLOCATOR: AccountingAlloc = AccountingAlloc::new();
//!
//! fn main() {
//!     let AllocCounts { alloc, dealloc } = GLOBAL_ALLOCATOR.count();
//!     println!("alloc {alloc} dealloc {dealloc}");
//! }
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt;
use std::fmt::{Debug, Display};
use std::panic::catch_unwind;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use once_cell::race::OnceBox;
use once_cell::unsync::Lazy;

#[derive(Default)]
/// A global memory allocator wrapper which counts allocated and deallocated bytes.
pub struct AccountingAlloc<A = System> {
    thread_counters: OnceBox<ThreadCounters>,
    allocator: A,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
/// A count of allocated and deallocated bytes returned by [`AccountingAlloc::count`].
pub struct AllocCounts {
    /// Count of allocated bytes.
    pub alloc: usize,
    /// Count of deallocated bytes.
    pub dealloc: usize,
}

#[derive(Debug)]
struct ThreadCounters {
    tx: Sender<Arc<ThreadCounter>>,
    shared: Mutex<ThreadCountersShared>,
}

#[derive(Debug)]
struct ThreadCountersShared {
    rx: Receiver<Arc<ThreadCounter>>,
    counters: Vec<Arc<ThreadCounter>>,
    dead: AllocCounts,
}

#[derive(Debug, Default)]
struct ThreadCounter {
    alloc: AtomicUsize,
    dealloc: AtomicUsize,
}

#[derive(Clone, Copy, Debug)]
enum ThreadCounterState {
    Uninitialized,
    Initializing(AllocCounts),
    Initialized,
}

impl AccountingAlloc<System> {
    /// Create a new [`AccountingAlloc`] using the [`System`] allocator.
    pub const fn new() -> Self {
        Self::with_allocator(System)
    }
}

impl<A> AccountingAlloc<A> {
    /// Create a new [`AccountingAlloc`] using the given allocator `A`.
    ///
    /// Note that in order for `AccountingAlloc<A>` to implement [`GlobalAlloc`], `A` must implement [`GlobalAlloc`].
    pub const fn with_allocator(allocator: A) -> Self {
        Self { thread_counters: OnceBox::new(), allocator }
    }

    /// Count the number of bytes allocated and deallocated globally.
    pub fn count(&self) -> AllocCounts {
        let thread_counters = self.thread_counters.get_or_init(Default::default);
        thread_counters.shared.lock().unwrap().count()
    }

    /// Increment the current thread's (de)allocated bytes count by `alloc` and `dealloc`.
    pub fn inc(&self, mut alloc: usize, mut dealloc: usize) {
        use ThreadCounterState::{Initialized, Initializing, Uninitialized};

        thread_local! {
            static COUNTER: Lazy<Arc<ThreadCounter>> = Default::default();
            static STATE: Cell<ThreadCounterState> = Cell::new(Uninitialized);
        }

        let thread_counters = &self.thread_counters;

        // As of rust 1.65.0, panicking from GlobalAlloc methods is UB, so catch anything here. NB: catch_unwind
        // allocates internally in the unwinding case, so any panic here will likely recurse and cause a double-panic,
        // resulting in a process abort.
        let _ignore = catch_unwind(move || {
            match STATE.try_with(|state| state.get())? {
                Uninitialized => {
                    // Transition to an "initializing" state, to prevent infinite recursion when we allocate below.
                    STATE.try_with(|state| state.set(Initializing(AllocCounts::default())))?;

                    // NB: LocalKey::<T>::try_with also allocates internally when T has a destructor.
                    let counter = COUNTER.try_with(|counter| Arc::clone(counter))?;

                    // Transition to "initialized" state.
                    if let Initializing(init_counter) = STATE.try_with(|state| state.replace(Initialized))? {
                        alloc += init_counter.alloc;
                        dealloc += init_counter.dealloc;
                    }

                    counter.alloc.fetch_add(alloc, Relaxed);
                    counter.dealloc.fetch_add(dealloc, Relaxed);

                    let thread_counters = thread_counters.get_or_init(Default::default);
                    let _ignore = thread_counters.tx.send(counter);

                    Ok(())
                }

                Initializing(init_counts) => STATE.try_with(|state| {
                    state.set(Initializing(AllocCounts {
                        alloc: init_counts.alloc + alloc,
                        dealloc: init_counts.dealloc + dealloc,
                    }))
                }),

                // This function is called from dealloc() in the destructor for the thread-local `COUNTER`. We use
                // LocalKey::try_with here so we don't panic when that's the case.
                Initialized => COUNTER.try_with(|counter| {
                    counter.alloc.fetch_add(alloc, Relaxed);
                    counter.dealloc.fetch_add(dealloc, Relaxed);
                }),
            }
        });
    }
}

impl<A: Debug> Debug for AccountingAlloc<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountingAlloc")
            .field("thread_counters", &self.thread_counters.get())
            .field("allocator", &self.allocator)
            .finish()
    }
}

/// Display the number of bytes each live thread has allocated and deallocated, along with the global total.
impl<A> Display for AccountingAlloc<A> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let thread_counters = self.thread_counters.get_or_init(Default::default);
        let mut shared = thread_counters.shared.lock().unwrap();

        let AllocCounts { alloc, dealloc } = shared.count();

        for (thread_idx, thread_counter) in shared.counters.iter().enumerate() {
            let thread_alloc = thread_counter.alloc.load(Relaxed);
            let thread_dealloc = thread_counter.dealloc.load(Relaxed);
            writeln!(f, "Thread {thread_idx}: alloc {thread_alloc} dealloc {thread_dealloc}")?;
        }
        let total = alloc - dealloc;
        writeln!(f, "Total: {total} (alloc {alloc} dealloc {dealloc})")
    }
}

unsafe impl<A: GlobalAlloc> GlobalAlloc for AccountingAlloc<A> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        self.inc(layout.size(), 0);
        self.allocator.alloc(layout)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        self.inc(0, layout.size());
        self.allocator.dealloc(ptr, layout);
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        self.inc(layout.size(), 0);
        self.allocator.alloc_zeroed(layout)
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        self.inc(new_size, layout.size());
        self.allocator.realloc(ptr, layout, new_size)
    }
}

impl Default for ThreadCounters {
    fn default() -> Self {
        let (tx, rx) = unbounded();
        Self {
            tx,
            shared: Mutex::new(ThreadCountersShared { rx, counters: Vec::with_capacity(64), dead: Default::default() }),
        }
    }
}

impl ThreadCountersShared {
    fn count(&mut self) -> AllocCounts {
        let mut alloc = 0;
        let mut dealloc = 0;

        self.counters.retain_mut(|counter| match Arc::get_mut(counter) {
            Some(counter) => {
                self.dead.alloc += *counter.alloc.get_mut();
                self.dead.dealloc += *counter.dealloc.get_mut();
                false
            }
            None => {
                alloc += counter.alloc.load(Relaxed);
                dealloc += counter.dealloc.load(Relaxed);
                true
            }
        });

        for counter in self.rx.try_iter() {
            match Arc::try_unwrap(counter) {
                Ok(mut counter) => {
                    self.dead.alloc += *counter.alloc.get_mut();
                    self.dead.dealloc += *counter.dealloc.get_mut();
                }
                Err(counter) => {
                    alloc += counter.alloc.load(Relaxed);
                    dealloc += counter.dealloc.load(Relaxed);
                    self.counters.push(counter);
                }
            }
        }

        AllocCounts { alloc: alloc + self.dead.alloc, dealloc: dealloc + self.dead.dealloc }
    }
}

#[cfg(test)]
mod tests {
    use std::thread::spawn;

    use super::*;

    #[global_allocator]
    static GLOBAL_ALLOCATOR: AccountingAlloc = AccountingAlloc::new();

    #[test]
    fn display() {
        let threads = (0..9)
            .map(|idx| spawn(move || drop(Vec::<u8>::with_capacity(10000 * idx))))
            .collect::<Vec<_>>();
        threads.into_iter().for_each(|thread| thread.join().unwrap());

        println!("{GLOBAL_ALLOCATOR:?}");
        println!("{GLOBAL_ALLOCATOR}");
        println!("{GLOBAL_ALLOCATOR:?}");
    }

    #[test]
    fn count() {
        let threads = (0..9)
            .map(|idx| spawn(move || drop(Vec::<u8>::with_capacity(10000 * idx))))
            .collect::<Vec<_>>();
        threads.into_iter().for_each(|thread| thread.join().unwrap());

        println!("{GLOBAL_ALLOCATOR:?}");
        let AllocCounts { alloc, dealloc } = GLOBAL_ALLOCATOR.count();
        println!("Total: alloc {alloc} dealloc {dealloc}");
        println!("{GLOBAL_ALLOCATOR:?}");
    }
}
