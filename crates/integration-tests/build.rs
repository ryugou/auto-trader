fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=../../proto/graphrag.proto");
    tonic_build::configure()
        .build_client(false)
        .build_server(true)
        .compile_protos(&["../../proto/graphrag.proto"], &["../../proto"])?;
    Ok(())
}
