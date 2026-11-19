//! The TypeScript types of the wire, in `client/src/generated/`. The test fails while they are stale;
//! `ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-proto --features ts --test bindings` writes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rookery_proto::{auth, context, error, methods, rpc, stream};
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
    );
    let mut made = files(&scratch);
    let index: String = made
        .keys()
        .map(|name| format!("export * from \"./{}\";\n", name.trim_end_matches(".ts")))
        .collect();
    made.insert("index.ts".into(), index);
    let target = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../client/src/generated");
    if std::env::var_os("ROOKERY_UPDATE_DOCS").is_some() {
        let _ = std::fs::remove_dir_all(&target);
        std::fs::create_dir_all(&target).unwrap();
        for (name, text) in &made {
            std::fs::write(target.join(name), text).unwrap();
        }
    } else {
        assert!(
            files(&target) == made,
            "client/src/generated is stale: ROOKERY_UPDATE_DOCS=1 cargo test -p rookery-proto --features ts --test bindings"
        );
    }
    let _ = std::fs::remove_dir_all(&scratch);
}
