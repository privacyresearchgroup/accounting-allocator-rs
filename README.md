# accounting-allocator

A Rust global memory allocator wrapper which counts allocated and deallocated bytes, avoiding contention between
threads.

The accounting allocator avoids contention by using per-thread atomic counters. It incurs small one-time global and
per-thread initialization overhead.

[API Documentation](https://privacyresearchgroup.github.io/accounting-allocator-rs/public/accounting_allocator/)  
[Private Documentation](https://privacyresearchgroup.github.io/accounting-allocator-rs/private/accounting_allocator/)  

## Contributing Bug Reports

GitHub is the project's bug tracker. Please
[search](https://github.com/privacyresearchgroup/accounting-allocator-rs/issues) for similar existing issues before
[submitting a new one](https://github.com/privacyresearchgroup/accounting-allocator-rs/issues/new).

## License

Licensed under [MIT](https://opensource.org/licenses/MIT).
