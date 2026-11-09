//! lD Mixer: an unofficial Linux mixer for Audient iD interfaces.
//!
//! The library holds everything that is not UI: the protocol tables, the USB
//! transport, the mixer state model and its persistence.

pub mod device;
pub mod protocol;
