//! Linux Soundboard — GTK4 native entry point

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    linux_soundboard::bootstrap::run();
}
