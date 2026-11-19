//! `layout.get` and `layout.put`: a person's layouts of a shell, by name, with a revision.

use rookery_kernel::{Call, Level, Methods, RpcError};
use rookery_proto::facilities::{LayoutPut, LayoutRef, LayoutValue, Revision};

use crate::own_user;

fn name(name: Option<&str>) -> Result<String, RpcError> {
    let name = name.unwrap_or("default");
    if name.is_empty() || name.len() > 100 {
        return Err(RpcError::invalid("a layout's name is 1 to 100 bytes"));
    }
    Ok(name.to_owned())
}

pub fn register(methods: &mut Methods) {
    methods
        .add("layout.get", |call: Call, p: LayoutRef| async move {
            let found = call
                .store()
                .layout()
                .get(own_user(&call)?, &name(p.name.as_deref())?)
                .await?;
            Ok(match found {
                Some((value, revision)) => LayoutValue {
                    value: Some(value),
                    revision,
                },
                None => LayoutValue {
                    value: None,
                    revision: 0,
                },
            })
        })
        .summary("The caller's layout of a shell, and its revision.")
        .level(Level::Own);
    methods
        .add("layout.put", |call: Call, p: LayoutPut| async move {
            if serde_json::to_vec(&p.value).map(|v| v.len()).unwrap_or(0) > 1024 * 1024 {
                return Err(RpcError::invalid("a layout is at most 1 MiB of JSON"));
            }
            let revision = call.store().layout().put(own_user(&call)?, &name(p.name.as_deref())?, &p.value, p.revision).await?;
            Ok(Revision { revision })
        })
        .summary("Keeps the caller's layout. With revision, only if it is at that one; CONFLICT otherwise.")
        .level(Level::Own);
}
