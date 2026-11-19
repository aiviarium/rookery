//! A host's set-up: the fields of its profile, and the flow that asks for
//! them, one question at a time. Rookery's command line takes what it was
//! given and asks for the rest; nothing here draws.

pub mod event;
pub mod fields;
pub mod flow;
pub mod machine;
pub mod setup;
