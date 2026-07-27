#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// glibc grows a separate malloc arena per contending thread, up to eight per
/// core. This process runs around twenty threads (GTK, the SQLite worker, the
/// UI worker pool, hotkey listeners, audio), and the resulting arenas retain
/// far more than the live data needs: capping them at two measured 3.7 MB less
/// PSS at idle with no behavioural change.
///
/// Declared directly rather than pulling `libc` in as a direct dependency,
/// which would require regenerating the third-party notices for one call.
/// `M_ARENA_MAX` is -8 in glibc's `malloc.h`.
#[cfg(target_env = "gnu")]
fn limit_malloc_arenas() {
    const M_ARENA_MAX: i32 = -8;
    extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    // SAFETY: `mallopt` is a glibc entry point that only sets an allocator
    // tunable and returns a status code. It takes no pointers and has no
    // preconditions beyond being called from a live process; calling it first
    // in `main` also guarantees no other thread is allocating yet.
    unsafe {
        mallopt(M_ARENA_MAX, 2);
    }
}

#[cfg(not(target_env = "gnu"))]
fn limit_malloc_arenas() {}

fn main() {
    limit_malloc_arenas();
    linux_soundboard::bootstrap::run();
}
