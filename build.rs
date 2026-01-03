use std::io::Result;

fn main() -> Result<()> {
    // Compile protobuf definitions with serde support
    // This allows proto types to serialize directly to JSON
    prost_build::Config::new()
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .compile_protos(&["proto/datacube.proto"], &["proto/"])?;
    Ok(())
}
