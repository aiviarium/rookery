//! rookery serve on Postgres, through its edge: the leader lock, sign-in, /ws, /rpc, the socket and the
//! shell. Each test has a database of its own.

mod common;
mod edge;
mod oidc;
