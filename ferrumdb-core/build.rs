use std::io::Result;

fn main() -> Result<()> {
    prost_build::compile_protos(&["src/proto/store.proto", "src/proto/wal.proto"], &["src/proto"])?;
    Ok(())
}
