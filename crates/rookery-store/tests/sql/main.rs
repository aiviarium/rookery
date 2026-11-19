//! Raw SQL: plugins' roles and their isolation, views over their collections, statements and
//! transactions, SQL migrations, and the credential and its rotation. Roles are the Postgres server's,
//! so each test database names its own apart (`TestDb::role_tag`).

#[path = "../data/common/mod.rs"]
mod common;
mod fixtures;
mod migrations;
mod pure;
mod role;
mod secrev;
mod tx;
