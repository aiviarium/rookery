//! The TypeScript types of the wire, in `client/src/generated/`. The test fails while they are stale;
//! `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-proto --features ts --test bindings` writes them.
//!
//! The folder also holds types ported from the prototype that have no Rust type here yet. This test
//! owns only the files it writes, and `index.ts`, which exports every file in the folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rookery_proto::{access, auth, context, error, facilities, jobs, methods, rpc, stream};
use ts_rs::{Config, TS};

macro_rules! export {
    ($config:expr, $($t:ty),* $(,)?) => {{
        $(<$t as TS>::export_all($config).unwrap_or_else(|e| panic!("exporting {}: {e}", stringify!($t)));)*
    }};
}

fn files(dir: &Path) -> BTreeMap<String, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "ts"))
        .map(|path| {
            (
                path.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&path).unwrap(),
            )
        })
        .collect()
}

#[test]
fn typescript_bindings() {
    let scratch = std::env::temp_dir().join(format!("rookery-bindings-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let config = Config::new()
        .with_out_dir(&scratch)
        .with_large_int("number");
    export!(
        &config,
        rookery_proto::Empty,
        rookery_proto::IdRef,
        rpc::Id,
        rpc::Request,
        rpc::Notification,
        rpc::Response,
        error::ErrorCode,
        error::ErrorData,
        error::ErrorBody,
        error::RpcError,
        context::User,
        context::PrincipalKind,
        context::Principal,
        stream::StreamCredit,
        stream::StreamClose,
        stream::StreamEof,
        stream::StreamOpened,
        methods::Level,
        methods::Grant,
        auth::Me,
        auth::AuthMode,
        auth::DeviceInfo,
        auth::DeviceList,
        auth::TokenCreate,
        auth::TokenInfo,
        auth::TokenCreated,
        auth::TokenList,
        auth::LinkKind,
        auth::LinkCreate,
        auth::LinkInfo,
        auth::LinkCreated,
        auth::LinkList,
        auth::SignedOut,
        auth::Sso,
        facilities::SettingScope,
        facilities::Scope,
        facilities::SettingRef,
        facilities::Setting,
        facilities::SettingValue,
        facilities::SettingsOf,
        facilities::SettingList,
        facilities::SettingSet,
        facilities::SettingReset,
        facilities::SecretRef,
        facilities::SecretSet,
        facilities::SecretRename,
        facilities::SecretValue,
        facilities::SecretsOf,
        facilities::SecretInfo,
        facilities::SecretList,
        facilities::StorageRef,
        facilities::StorageSet,
        facilities::StorageList,
        facilities::StorageEntry,
        facilities::StorageValue,
        facilities::StorageEntries,
        facilities::Revision,
        facilities::Deleted,
        facilities::ConnectionType,
        facilities::ConnectionTypes,
        facilities::ConnectionInfo,
        facilities::ConnectionsOf,
        facilities::ConnectionList,
        facilities::ConnectionCreate,
        facilities::ConnectionUpdate,
        facilities::ConnectionResolve,
        facilities::ConnectionResolved,
        facilities::ConnectionTested,
        facilities::BlobPut,
        facilities::BlobInfo,
        facilities::LayoutRef,
        facilities::LayoutValue,
        facilities::LayoutPut,
        access::AccessGrant,
        access::AccessRevoke,
        access::ResourceRef,
        access::GrantInfo,
        access::GrantList,
        access::LevelHeld,
        access::LevelOf,
        access::AccessRequire,
        access::AccessFilter,
        access::Resources,
        access::UserInfo,
        access::UserList,
        access::SetRole,
        access::SetDisabled,
        access::GroupInfo,
        access::GroupList,
        access::GroupCreate,
        access::Membership,
        access::AuditQuery,
        access::AuditEntry,
        access::AuditList,
        jobs::JobState,
        jobs::JobEnqueue,
        jobs::JobInfo,
        jobs::JobRef,
        jobs::JobQuery,
        jobs::JobList,
        jobs::JobFollow,
        jobs::LogItem,
        jobs::JobEnd,
        jobs::LogItems,
        jobs::JobLog,
        jobs::JobProgress,
        jobs::LeaseAcquire,
        jobs::LeaseRef,
        jobs::LeaseInfo,
        jobs::LeaseState,
        jobs::TopicPublish,
        jobs::Published,
        jobs::TopicSubscribe,
        jobs::TopicMessage,
        jobs::TopicDelivery,
        jobs::Nacked,
        jobs::TopicRef,
        jobs::SubscriptionInfo,
        jobs::TopicInfo,
        jobs::DeadLetter,
        jobs::DeadLetters,
        jobs::EventPublish,
        jobs::EventSubscribe,
        jobs::EventSubscribed,
        jobs::EventUnsubscribe,
        jobs::EventNotice,
        jobs::EventsDropped,
        jobs::Delivered,
        jobs::NotifySend,
        jobs::NotificationInfo,
        jobs::NotifySent,
        jobs::NotifyQuery,
        jobs::NotificationList,
        jobs::NotifyRead,
        jobs::Count,
        jobs::NotifyAnswer,
        jobs::NotifyPref,
    );
    let made = files(&scratch);
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../client/src/generated");
    let update = std::env::var_os("ROOKERY_UPDATE_DOCS").is_some();
    if update {
        std::fs::create_dir_all(&target).unwrap();
        for (name, text) in &made {
            std::fs::write(target.join(name), text).unwrap();
        }
    }
    let kept = files(&target);
    let index = index(kept.keys().chain(made.keys()));
    if update {
        std::fs::write(target.join("index.ts"), &index).unwrap();
    }
    let kept = files(&target);
    let stale: Vec<&String> = made
        .iter()
        .filter(|(name, text)| kept.get(*name) != Some(text))
        .map(|(name, _)| name)
        .collect();
    assert!(
        stale.is_empty() && kept.get("index.ts") == Some(&index),
        "client/src/generated is stale ({stale:?}): ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-proto --features ts --test bindings"
    );
    let _ = std::fs::remove_dir_all(&scratch);
}

/// `index.ts`: every type file of the folder, once, in order.
fn index<'a>(names: impl Iterator<Item = &'a String>) -> String {
    let names: std::collections::BTreeSet<&str> = names
        .map(|n| n.trim_end_matches(".ts"))
        .filter(|n| *n != "index")
        .collect();
    let mut out = String::from(
        "// The wire types of rookery serve, written by crates/rookery-proto/tests/bindings.rs.\n",
    );
    for name in names {
        out.push_str(&format!("export type * from \"./{name}\"\n"));
    }
    out
}
