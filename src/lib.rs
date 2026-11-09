//! lD Mixer: an unofficial Linux mixer for Audient iD interfaces.
//!
//! The library holds everything that is not UI: the protocol tables, the USB
//! transport (and a mock device), the mixer state model, persistence and the
//! background device engine.

pub mod config;
pub mod device;
pub mod engine;
pub mod mixer;
pub mod mock;
pub mod protocol;
pub mod vsource;
