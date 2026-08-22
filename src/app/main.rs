#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
