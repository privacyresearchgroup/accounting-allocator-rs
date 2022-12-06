//! Basic functionality tests using `AccountingAlloc` as the global allocator.
//!
//! Since it's hard to pin down exactly how much will be allocated using the global allocator in a Rust program, as Rust
//! is allowed to (and does) allocate whatever it needs using the global allocator internally, the tests in this module
//! mostly exist to demonstrate that the `Display` and `Debug` impls for `AccountingAlloc` don't hang, infinitely
//! recurse, panic, or abort.
//!
//! On most hosts, there are some implicit assertions in the system allocator for double-free and leak detection. (Plus,
//! we could probably explicitly add a memory sanitizer on top of that later.) There are also potentially some code
//! paths which `abort()` inside `AccountingAlloc`, which would indicate bugs.

use accounting_allocator::{AccountingAlloc, AllTimeAllocStats};

use crossbeam_utils::thread::scope;

#[global_allocator]
static GLOBAL_ALLOCATOR: AccountingAlloc = AccountingAlloc::new();

#[test]
fn global_display() {
    scope(|scope| (0..9).for_each(|idx| drop(scope.spawn(move |_| drop(Vec::<u8>::with_capacity(10000 * idx))))))
        .unwrap();

    println!("{GLOBAL_ALLOCATOR:?}");
    println!("{GLOBAL_ALLOCATOR}");
    println!("{GLOBAL_ALLOCATOR:?}");
}

#[test]
fn global_count() {
    scope(|scope| (0..9).for_each(|idx| drop(scope.spawn(move |_| drop(Vec::<u8>::with_capacity(10000 * idx))))))
        .unwrap();

    println!("{GLOBAL_ALLOCATOR:?}");
    let AllTimeAllocStats { alloc, dealloc, largest_alloc } = GLOBAL_ALLOCATOR.count().all_time;
    println!("Total: alloc {alloc} dealloc {dealloc} largest_alloc {largest_alloc}");
    println!("{GLOBAL_ALLOCATOR:?}");
}
