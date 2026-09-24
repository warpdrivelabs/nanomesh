//! 编译 `proto/nm.proto` 为 Rust（prost）。需要系统 `protoc`（`brew install protobuf`）。
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=proto/nm.proto");
    prost_build::compile_protos(&["proto/nm.proto"], &["proto"])?;
    Ok(())
}
