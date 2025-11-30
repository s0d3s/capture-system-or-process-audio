# Dependency Notices

The original core is MIT OR Apache-2.0. Dependencies retain their own licenses.

| Dependency | Use | License | Upstream |
| --- | --- | --- | --- |
| allocation-counter 0.8.1 | Test-only current-thread allocation observation | MIT OR Apache-2.0 | https://github.com/fornwall/allocation-counter |
| crossbeam-utils 0.8.23 | Guarded lock-free atomic owner exchange | MIT OR Apache-2.0 | https://github.com/crossbeam-rs/crossbeam |

The observer replaces the allocator only in test executables that link it. It does
not observe native allocators or allocations on other threads; each measured thread
needs its own measurement scope. Measured closures must return normally. Production
builds do not use this allocator. Exact versions and checksums are in Cargo.lock.