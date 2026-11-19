//! The data layer's tests: collections, queries, writes, references, the change feed, search and the
//! home screen's recents, pins and activity. Each that needs Postgres runs on a database of its own
//! and is skipped without `ROOKERY_TEST_POSTGRES`.

mod common;
mod constraints;
mod data_reads;
mod data_writes;
mod feed;
mod home;
mod queries;
mod refs;
mod richtext;
mod schema;
mod system;
mod values;
