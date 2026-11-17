//! What rookery says when two of its programs are of different versions: a
//! client and its server, or the server and a host's controller. Each is one
//! sentence that names both versions and what to update, the same for the
//! command line and the screens.

use crate::{API, HostType, VERSION};

/// The lowest `api` of a host's controller this rookery can work with.
pub const NEEDS_HOST_API: u32 = 2;

/// Where updating from an older rookery is explained.
pub const UPGRADE_DOCS: &str =
    "https://github.com/aiviarium/rookery/blob/main/docs/content/docs/(guide)/use/upgrading.mdx";

/// The operations of rookery 0.5 that this server does not have, which only a
/// client of that version still calls.
pub const OLDER_OPS: &[&str] = &["tasks.list", "tasks.run", "tasks.choices", "actions.act"];

/// Why a client refuses to work with the server at `url`, which says it is
/// rookery `version` of `api`; `None` when the server is new enough.
pub fn older_server(url: &str, version: &str, api: u32) -> Option<String> {
    (api < API).then(|| {
        format!(
            "The rookery server at {url} is rookery {version} (API {api}), older than this rookery {VERSION} (API {API}), and would not understand what it is sent. Update the server first, then the clients and the hosts: {UPGRADE_DOCS}"
        )
    })
}

/// What updates the controller on a host of this type.
pub fn controller_update(host: &str, host_type: HostType) -> String {
    match host_type {
        HostType::Workstation | HostType::Node => format!("rookery apply {host}"),
        HostType::Managed | HostType::Ssh => format!("rookery run {host} install-controller"),
    }
}

/// What a host says when its controller is rookery `version` of `api`, too
/// old for this rookery; `None` when it is new enough.
pub fn older_controller(
    host: &str,
    version: &str,
    api: u32,
    host_type: HostType,
) -> Option<String> {
    (api < NEEDS_HOST_API).then(|| controller_is_older(host, version, host_type))
}

/// That the controller on `host`, rookery `version`, is too old for this rookery,
/// and what updates it.
pub fn controller_is_older(host: &str, version: &str, host_type: HostType) -> String {
    format!(
        "The controller on {host} is rookery {version}, older than this rookery {VERSION}. Update it with {}",
        controller_update(host, host_type)
    )
}

/// What the server answers a client that calls an operation it does not
/// have: for one of an older rookery, that the client is older.
pub fn no_operation(name: &str) -> String {
    match OLDER_OPS.contains(&name) {
        true => format!(
            "No operation {name}: it is a call of rookery 0.5, and this server is rookery {VERSION}. Update rookery where this call comes from: {UPGRADE_DOCS}"
        ),
        false => format!("No operation {name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_server_is_named_with_both_versions() {
        assert_eq!(older_server("https://pier.example", VERSION, API), None);
        let said = older_server("https://pier.example", "0.5.2", 1).unwrap();
        assert!(said.contains("is rookery 0.5.2 (API 1)"), "{said}");
        assert!(
            said.contains(&format!("this rookery {VERSION} (API {API})")),
            "{said}"
        );
        assert!(said.contains("Update the server first"), "{said}");
        assert!(said.ends_with(UPGRADE_DOCS), "{said}");
    }

    #[test]
    fn an_older_controller_says_what_updates_it() {
        assert_eq!(
            older_controller("studio", VERSION, NEEDS_HOST_API, HostType::Workstation),
            None
        );
        let said = older_controller("studio", "0.5.2", 1, HostType::Workstation).unwrap();
        assert!(
            said.starts_with("The controller on studio is rookery 0.5.2, older than this rookery")
        );
        assert!(
            said.ends_with("Update it with rookery apply studio"),
            "{said}"
        );
        let said = older_controller("big", "0.5.2", 1, HostType::Node).unwrap();
        assert!(said.ends_with("rookery apply big"), "{said}");
        let said = older_controller("pier", "0.5.2", 1, HostType::Managed).unwrap();
        assert!(
            said.ends_with("rookery run pier install-controller"),
            "{said}"
        );
    }

    #[test]
    fn a_call_of_an_older_client_says_so() {
        assert_eq!(no_operation("hosts.nothing"), "No operation hosts.nothing");
        assert!(no_operation("tasks.list").contains("a call of rookery 0.5"));
        // None of them is an operation now.
        for name in OLDER_OPS {
            assert!(
                crate::ops::find(crate::ops::SERVE, name).is_none(),
                "{name}"
            );
            assert!(crate::ops::find(crate::ops::HOST, name).is_none(), "{name}");
        }
    }
}
