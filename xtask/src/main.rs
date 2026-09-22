//! `xtask` — 开发者任务编排：proto 生成、多节点 e2e、移动端初始化。
//! 用法：`cargo run -p xtask -- <task>`

fn main() -> anyhow::Result<()> {
    let task = std::env::args().nth(1).unwrap_or_default();
    match task.as_str() {
        "proto" => println!("TODO: 用 prost-build 生成协议代码"),
        "e2e" => println!("TODO: 启动多节点集成测试"),
        "mobile-init" => println!("TODO: 在 clients/app 下执行 tauri android/ios init"),
        _ => println!("usage: cargo run -p xtask -- <proto|e2e|mobile-init>"),
    }
    Ok(())
}
