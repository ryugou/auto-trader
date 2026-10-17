#![doc = "Build script that compiles the GraphRAG protobuf definitions into client code."]

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tonic_build::configure()
        .build_server(false)
        .compile_protos(&["../../proto/graphrag.proto"], &["../../proto"])?;
    Ok(())
}
