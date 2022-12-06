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

/// Make some variously sized test allocations in separate threads and return their sizes.
fn test_allocations() -> Vec<usize> {
    let vec_sizes: Vec<usize> = (0..9).map(|idx| 10000 * idx).collect();

    scope(|scope| {
        for vec_size in &vec_sizes {
            drop(scope.spawn(move |_| drop(Vec::<u8>::with_capacity(*vec_size))))
        }
    })
    .unwrap();

    vec_sizes
}

#[test]
fn global_display() {
    test_allocations();

    println!("{GLOBAL_ALLOCATOR:?}");
    println!("{GLOBAL_ALLOCATOR}");
    println!("{GLOBAL_ALLOCATOR:?}");
}

#[test]
fn global_count() {
    let vec_sizes = test_allocations();

    println!("{GLOBAL_ALLOCATOR:?}");

    let stats = GLOBAL_ALLOCATOR.count();

    // Assert that at least the amounts allocated above were counted.
    assert!(stats.all_time.alloc >= vec_sizes.iter().sum());
    assert!(stats.all_time.dealloc >= vec_sizes.iter().sum());
    assert!(stats.all_time.largest_alloc >= *vec_sizes.iter().max().unwrap());

    let AllTimeAllocStats { alloc, dealloc, largest_alloc } = stats.all_time;
    println!("Total: alloc {alloc} dealloc {dealloc} largest_alloc {largest_alloc}");

    println!("{GLOBAL_ALLOCATOR:?}");
}
