//! What rookery says when two of its programs are of different versions: a
//! client and its server, or the server and a host's controller. Each is one
//! sentence that names both versions and what to update, the same for the
//! command line and the screens.

use crate::{API, HostType, VERSION};

/// The lowest `api` of a host's controller this rookery can work with.
pub const NEEDS_HOST_API: u32 = 2;

/// Where updating from an older rookery is explained.
pub const UPGRADE_DOCS: &str =
    "https://github.com/aiviarium/rookery/blob/main/docs/content/docs/(guide)/use/updates.mdx";

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_older_server_is_named_with_both_versions() {
        assert_eq!(older_server("https://annex.example", VERSION, API), None);
        let said = older_server("https://annex.example", "0.5.2", 1).unwrap();
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
        let said = older_controller("annex", "0.5.2", 1, HostType::Managed).unwrap();
        assert!(
            said.ends_with("rookery run annex install-controller"),
            "{said}"
        );
    }
}
