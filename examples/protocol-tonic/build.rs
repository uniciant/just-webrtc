use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_path: &Path = "proto/main.proto".as_ref();
    let target = std::env::var("TARGET").unwrap();
    // directory the main .proto file resides in
    let proto_dir = proto_path
        .parent()
        .expect("proto file should reside in a directory");
    tonic_build::configure()
        .build_transport(target != "wasm32-unknown-unknown")
        .compile(&[proto_path], &[proto_dir])?;
    Ok(())
}
