//! Generates the gRPC service from `proto/rag2.proto` when the `grpc` feature is on.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(feature = "grpc")]
    {
        println!("cargo:rerun-if-changed=proto/rag2.proto");
        tonic_prost_build::configure()
            .build_client(true)
            .compile_protos(&["proto/rag2.proto"], &["proto"])?;
    }
    Ok(())
}
