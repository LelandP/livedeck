//! How much memory the Rust side of the app holds, logged once a minute.
//!
//! The process's memory also counts libmpv, FFmpeg, Slint's renderer and the GPU driver. When it
//! grows, this line says whether Rust code is the one holding on (a collection or queue that only
//! grows) or a native library is: the allocator below counts every Rust allocation and free. It
//! costs two relaxed atomic adds per call.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

/// The system allocator, counting the bytes Rust code currently holds.
pub struct Counting;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: every call goes straight to `System`; the counters are only statistics.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) };
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { System.realloc(ptr, layout, new_size) };
        if !new.is_null() {
            LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
            LIVE_BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        new
    }
}

/// Bytes Rust code holds right now.
pub fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed)
}

/// Logs the Rust heap once a minute, from a thread of its own, for as long as the app runs.
pub fn report_every_minute() {
    let spawned = std::thread::Builder::new()
        .name("heap-report".into())
        .spawn(|| loop {
            std::thread::sleep(Duration::from_secs(60));
            tracing::info!(
                rust_heap_mb = live_bytes() / (1024 * 1024),
                allocations = ALLOCATIONS.load(Ordering::Relaxed),
                "memory"
            );
        });
    if let Err(err) = spawned {
        tracing::warn!(%err, "could not start the memory report");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_kept_allocation_is_counted() {
        // Tests run on parallel threads that allocate too, so only the 8 MiB this one holds is
        // checked for, with room for the others.
        let before = live_bytes();
        let kept = vec![1u8; 8 * 1024 * 1024];
        assert!(live_bytes() >= before.saturating_sub(2 * 1024 * 1024) + 6 * 1024 * 1024);
        drop(kept);
    }
}
