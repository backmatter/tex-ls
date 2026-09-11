//! Optional allocator counters. Disabled in production builds.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

struct CountingAllocator;

static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);
static TOTAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static PEAK_BYTES: AtomicUsize = AtomicUsize::new(0);

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Delegating the request unchanged preserves `System`'s contract.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            add_live(layout.size());
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: Delegating the request unchanged preserves `System`'s contract.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            add_live(layout.size());
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
        // SAFETY: `ptr` and `layout` come from the matching allocator call.
        unsafe { System.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: Delegating the request unchanged preserves `System`'s contract.
        let new_ptr = unsafe { System.realloc(ptr, layout, new_size) };
        if !new_ptr.is_null() {
            TOTAL_BYTES.fetch_add(new_size, Ordering::Relaxed);
            let delta = new_size as isize - layout.size() as isize;
            let live = LIVE_BYTES.fetch_add(delta, Ordering::Relaxed) + delta;
            update_peak(live.max(0) as usize);
        }
        new_ptr
    }
}

fn add_live(bytes: usize) {
    TOTAL_BYTES.fetch_add(bytes, Ordering::Relaxed);
    let live = LIVE_BYTES.fetch_add(bytes as isize, Ordering::Relaxed) + bytes as isize;
    update_peak(live.max(0) as usize);
}

fn update_peak(candidate: usize) {
    let mut peak = PEAK_BYTES.load(Ordering::Relaxed);
    while candidate > peak {
        match PEAK_BYTES.compare_exchange_weak(
            peak,
            candidate,
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => peak = actual,
        }
    }
}

fn live_bytes() -> usize {
    LIVE_BYTES.load(Ordering::Relaxed).max(0) as usize
}

pub fn sample() -> [usize; 3] {
    [
        live_bytes(),
        PEAK_BYTES.load(Ordering::Relaxed),
        TOTAL_BYTES.load(Ordering::Relaxed),
    ]
}
