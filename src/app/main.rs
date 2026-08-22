#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// glibc grows a malloc arena per contending thread, up to eight per core. We
/// run ~20 threads and the arenas hold far more than the live data needs;
/// capping at two measured 3.7 MB less PSS at idle.
///
/// Declared by hand — one call isn't worth taking `libc` as a direct dep and
/// regenerating the third-party notices. `M_ARENA_MAX` is -8 in malloc.h.
#[cfg(target_env = "gnu")]
fn limit_malloc_arenas() {
    const M_ARENA_MAX: i32 = -8;
    extern "C" {
        fn mallopt(param: i32, value: i32) -> i32;
    }
    // SAFETY: allocator tunable, no pointers. First call in main, so nothing
    // else is allocating yet.
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
