//! The core of `rookery serve`.
//!
//! Each facility has a `register(methods: &mut Methods)` that adds its methods, each with the level a
//! caller needs and the schema of its params. `rookery-server` builds the [`Kernel`] from them and serves
//! it on `/ws`, `/rpc` and the Unix socket.

pub mod auth;
pub mod config;
pub mod conn;
pub mod kernel;
pub mod methods;
pub mod streams;
#[cfg(feature = "testing")]
pub mod testing;

pub use config::Config;
pub use kernel::Kernel;
pub use methods::{Call, Caller, Methods};
pub use rookery_proto::methods::{Grant, Level};
pub use rookery_proto::{Empty, RpcError};

pub use rookery_api::VERSION;
