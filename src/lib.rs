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
//!     let AllocCounts { alloc, dealloc, largest_alloc } = GLOBAL_ALLOCATOR.count();
//!     println!("alloc {alloc} dealloc {dealloc} largest_alloc {largest_alloc}");
//! }
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::fmt;
use std::fmt::{Debug, Display};
use std::panic::catch_unwind;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering::{AcqRel, Relaxed};
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
/// Statistics for allocations and deallocations made with an [`AccountingAlloc`].
pub struct AllocCounts {
    /// Count of all bytes ever allocated with the allocator, globally.
    pub alloc: usize,
    /// Count of all bytes ever deallocated with the allocator, globally.
    pub dealloc: usize,
    /// Largest allocation size in bytes since the last call to [`AccountingAlloc::count`].
    pub largest_alloc: usize,
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
    largest_alloc: AtomicUsize,
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

    /// Return the latest statistics for this allocator.
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
                    let mut largest_alloc = alloc;
                    if let Initializing(init_counter) = STATE.try_with(|state| state.replace(Initialized))? {
                        alloc += init_counter.alloc;
                        dealloc += init_counter.dealloc;
                        largest_alloc = largest_alloc.max(init_counter.largest_alloc);
                    }

                    counter.alloc.fetch_add(alloc, Relaxed);
                    counter.dealloc.fetch_add(dealloc, Relaxed);
                    counter.largest_alloc.fetch_max(largest_alloc, AcqRel);

                    let thread_counters = thread_counters.get_or_init(Default::default);
                    let _ignore = thread_counters.tx.send(counter);

                    Ok(())
                }

                Initializing(init_counts) => STATE.try_with(|state| {
                    state.set(Initializing(AllocCounts {
                        alloc: init_counts.alloc + alloc,
                        dealloc: init_counts.dealloc + dealloc,
                        largest_alloc: init_counts.largest_alloc.max(alloc),
                    }))
                }),

                // This function is called from dealloc() in the destructor for the thread-local `COUNTER`. We use
                // LocalKey::try_with here so we don't panic when that's the case.
                Initialized => COUNTER.try_with(|counter| {
                    counter.alloc.fetch_add(alloc, Relaxed);
                    counter.dealloc.fetch_add(dealloc, Relaxed);
                    counter.largest_alloc.fetch_max(alloc, AcqRel);
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

        let AllocCounts { alloc, dealloc, largest_alloc } = shared.count();

        for (thread_idx, thread_counter) in shared.counters.iter().enumerate() {
            let thread_alloc = thread_counter.alloc.load(Relaxed);
            let thread_dealloc = thread_counter.dealloc.load(Relaxed);
            writeln!(f, "Thread {thread_idx}: alloc {thread_alloc} dealloc {thread_dealloc}")?;
        }
        let total = alloc - dealloc;
        writeln!(
            f,
            "Total: {total} (alloc {alloc} dealloc {dealloc} largest_alloc {largest_alloc})"
        )
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
        let mut largest_alloc = 0;
        self.dead.largest_alloc = 0;

        self.counters.retain_mut(|counter| match Arc::get_mut(counter) {
            Some(counter) => {
                self.dead.alloc += *counter.alloc.get_mut();
                self.dead.dealloc += *counter.dealloc.get_mut();
                self.dead.largest_alloc = self.dead.largest_alloc.max(*counter.largest_alloc.get_mut());
                false
            }
            None => {
                alloc += counter.alloc.load(Relaxed);
                dealloc += counter.dealloc.load(Relaxed);
                largest_alloc = largest_alloc.max(counter.largest_alloc.swap(0, AcqRel));
                true
            }
        });

        for counter in self.rx.try_iter() {
            match Arc::try_unwrap(counter) {
                Ok(mut counter) => {
                    self.dead.alloc += *counter.alloc.get_mut();
                    self.dead.dealloc += *counter.dealloc.get_mut();
                    self.dead.largest_alloc = self.dead.largest_alloc.max(*counter.largest_alloc.get_mut());
                }
                Err(counter) => {
                    alloc += counter.alloc.load(Relaxed);
                    dealloc += counter.dealloc.load(Relaxed);
                    largest_alloc = largest_alloc.max(counter.largest_alloc.swap(0, AcqRel));
                    self.counters.push(counter);
                }
            }
        }

        AllocCounts {
            alloc: alloc + self.dead.alloc,
            dealloc: dealloc + self.dead.dealloc,
            largest_alloc: largest_alloc.max(self.dead.largest_alloc),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::identity;

    use crossbeam_utils::thread::scope;

    use super::*;

    #[derive(Default)]
    struct TestAlloc;

    struct Allocation {
        layout: Layout,
    }

    struct AllocationHandle<'a> {
        allocator: &'a AccountingAlloc<TestAlloc>,
        ptr: *mut u8,
        layout: Layout,
    }

    fn test_allocations<'a, T>(
        allocator: &'a AccountingAlloc<TestAlloc>,
        allocate: fn(&'a AccountingAlloc<TestAlloc>, Layout) -> AllocationHandle<'a>,
        callback: impl FnOnce(Vec<AllocationHandle<'a>>) -> T,
    ) -> T {
        let layouts: Vec<_> = (1..10).map(|idx| Layout::array::<u8>(10000 * idx).unwrap()).collect();
        let (allocations_tx, allocations_rx) = unbounded();
        scope(|scope| {
            for layout in layouts.clone() {
                let allocations_tx = allocations_tx.clone();
                scope.spawn(move |_scope| allocations_tx.send(allocate(allocator, layout)).unwrap());
            }
            drop(allocations_tx);
            callback(allocations_rx.into_iter().collect())
        })
        .unwrap()
    }

    fn expected_counts<'a>(allocations: &[AllocationHandle<'a>]) -> AllocCounts {
        let allocation_sizes = allocations.iter().map(|allocation| allocation.layout.size());
        AllocCounts {
            alloc: allocation_sizes.clone().sum::<usize>(),
            dealloc: 0,
            largest_alloc: allocation_sizes.max().unwrap(),
        }
    }

    #[test]
    fn alloc() {
        let allocator = Default::default();
        let (_allocations, expected) = test_allocations(&allocator, AllocationHandle::new, |allocations| {
            // test `allocator.count()` while threads are still alive.
            let expected = expected_counts(&allocations);
            assert_eq!(allocator.count(), expected);
            (allocations, expected)
        });

        // test `allocator.count()` again after the threads are dead.
        assert_eq!(allocator.count(), AllocCounts { largest_alloc: 0, ..expected });
    }

    #[test]
    fn dealloc() {
        let allocator = &Default::default();
        let allocations = test_allocations(&allocator, AllocationHandle::new, identity);
        let expected = expected_counts(&allocations);

        assert_eq!(allocator.count(), expected);

        scope(|scope| {
            for allocation in allocations {
                scope.spawn(move |_scope| drop(allocation));
            }
        })
        .unwrap();

        assert_eq!(
            allocator.count(),
            AllocCounts { dealloc: expected.alloc, largest_alloc: 0, ..expected }
        );
    }

    #[test]
    fn alloc_zeroed() {
        let allocator = &Default::default();
        let allocations = test_allocations(&allocator, AllocationHandle::new_zeroed, identity);
        let expected = expected_counts(&allocations);

        assert_eq!(allocator.count(), expected);
    }

    #[test]
    fn realloc() {
        let allocator = &Default::default();
        let mut allocations = test_allocations(&allocator, AllocationHandle::new, identity);
        let expected = expected_counts(&allocations);

        assert_eq!(allocator.count(), expected);

        scope(|scope| {
            for allocation in &mut allocations {
                scope.spawn(move |_scope| allocation.realloc(allocation.layout.size() * 2));
            }
        })
        .unwrap();

        assert_eq!(
            allocator.count(),
            AllocCounts {
                alloc: expected.alloc * 3,
                dealloc: expected.alloc,
                largest_alloc: expected.largest_alloc * 2
            }
        );
    }

    unsafe impl GlobalAlloc for TestAlloc {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            Box::into_raw(Box::new(Allocation { layout })) as *mut u8
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            assert_eq!(layout, Box::from_raw(ptr as *mut Allocation).layout);
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            self.alloc(layout)
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            self.dealloc(ptr, layout);
            self.alloc(Layout::from_size_align_unchecked(new_size, layout.align()))
        }
    }

    impl<'a> AllocationHandle<'a> {
        fn new(allocator: &'a AccountingAlloc<TestAlloc>, layout: Layout) -> Self {
            Self { allocator, ptr: unsafe { allocator.alloc(layout) }, layout }
        }

        fn new_zeroed(allocator: &'a AccountingAlloc<TestAlloc>, layout: Layout) -> Self {
            Self { allocator, ptr: unsafe { allocator.alloc_zeroed(layout) }, layout }
        }

        fn realloc(&mut self, new_size: usize) {
            unsafe {
                self.ptr = self.allocator.realloc(self.ptr, self.layout, new_size);
                self.layout = Layout::from_size_align_unchecked(new_size, self.layout.align());
            }
        }
    }

    unsafe impl Send for AllocationHandle<'_> {}

    impl Drop for AllocationHandle<'_> {
        fn drop(&mut self) {
            unsafe { self.allocator.dealloc(self.ptr, self.layout) };
        }
    }
}
