//! A client for DFHack's remote RPC protocol, aimed at the RemoteFortressReader
//! plugin that ships with DFHack.
//!
//! Protobuf definitions and protocol layout are vendored from DFHack 53.16-r1.1.

pub mod client;
pub mod methods;
pub mod protocol;

pub use client::{Client, Method};

/// Generated protobuf types, one module per protobuf package.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/_protos.rs"));
}

/// Shorthand for the RemoteFortressReader package.
pub use proto::remote_fortress_reader as rfr;
pub use proto::dfproto;
