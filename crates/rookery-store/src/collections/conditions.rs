//! Batch fencing conditions. Coordination APIs can grant these leases independently; checks run
//! within the write transaction and lock the grant until commit.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeaseCondition {
    pub key: String,
    pub token: i64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BatchCondition {
    pub lease: LeaseCondition,
}
