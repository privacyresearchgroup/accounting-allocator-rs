use accounting_allocator::{AccountingAlloc, AllocCounts};

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
    let AllocCounts { alloc, dealloc, largest_alloc } = GLOBAL_ALLOCATOR.count();
    println!("Total: alloc {alloc} dealloc {dealloc} largest_alloc {largest_alloc}");
    println!("{GLOBAL_ALLOCATOR:?}");
}
