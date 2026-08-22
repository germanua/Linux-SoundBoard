#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// glibc spins up a malloc arena per contending thread, up to eight per core.
/// We run ~20 threads (GTK, SQLite worker, UI pool, hotkey listeners, audio)
/// and those arenas hold far more than the live data needs. Capping at two
/// measured 3.7 MB less PSS at idle, no behaviour change.
///
/// Declared by hand rather than taking `libc` as a direct dep — one call is not
/// worth regenerating the third-party notices. `M_ARENA_MAX` is -8 in malloc.h.
#[cfg(target_env = "gnu")]
fn limit_malloc_arenas() {
    const M_ARENA_MAX: i32 = -8;
    extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    // SAFETY: `mallopt` just sets an allocator tunable and returns a status.
    // No pointers, no preconditions, and running it first in `main` means
    // nothing else is allocating yet.
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
