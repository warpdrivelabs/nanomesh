//! 编译 `proto/im.proto` 为 Rust（prost）。需要系统 `protoc`（`brew install protobuf`）。
fn main() -> std::io::Result<()> {
    println!("cargo:rerun-if-changed=proto/im.proto");
    prost_build::compile_protos(&["proto/im.proto"], &["proto"])?;
    Ok(())
}
