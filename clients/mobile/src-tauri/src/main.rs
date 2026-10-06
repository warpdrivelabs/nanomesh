// Windows release 隐藏控制台窗口
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    nmspace_mobile_lib::run();
}
