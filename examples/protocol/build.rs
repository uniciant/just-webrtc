use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_path: &Path = "proto/main.proto".as_ref();
    // directory the main .proto file resides in
    let proto_dir = proto_path
        .parent()
        .expect("proto file should reside in a directory");
    prost_build::Config::new()
        .file_descriptor_set_path("file_descriptor_set.bin")
        .enable_type_names()
        .compile_protos(&[proto_path], &[proto_dir])?;
    Ok(())
}
