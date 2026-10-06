fn main() {
    // frontendDist = ../ui 在编译期由 generate_context! 内嵌；tauri_build 默认不跟踪它，
    // 导致纯前端(ui/)改动不触发重编译、dev 里看到旧界面。显式声明后 ui/ 改动即触发重建。
    println!("cargo:rerun-if-changed=../ui");
    tauri_build::build();
}
