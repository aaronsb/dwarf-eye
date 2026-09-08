use std::io::Result;

/// Protobuf definitions vendored from DFHack at tag 53.16-r1.1.
const PROTOS: &[&str] = &[
    "proto/CoreProtocol.proto",
    "proto/Basic.proto",
    "proto/BasicApi.proto",
    "proto/ItemdefInstrument.proto",
    "proto/RemoteFortressReader.proto",
    "proto/AdventureControl.proto",
    "proto/DwarfControl.proto",
    "proto/ui_sidebar_mode.proto",
];

fn main() -> Result<()> {
    for p in PROTOS {
        println!("cargo:rerun-if-changed={p}");
    }
    // Let prost emit one include file so cross-package `super::` paths between
    // the generated modules resolve.
    let mut config = prost_build::Config::new();
    config.include_file("_protos.rs");
    config.compile_protos(PROTOS, &["proto"])
}
