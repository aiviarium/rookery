//! Provisioning inside `rookery serve`: the catalogue a plugin's provisioners
//! offer, a machine made in the same call as its host's profile, what each
//! provisioner says of itself in the list, and what keeps one from being
//! removed. The provisioner is a package of a made-up plugin whose command is
//! a shell script, so nothing reaches a real machine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rookery_kernel::methods::Caller;
use rookery_kernel::{Call, Config, Kernel, Methods, RpcError};
use rookery_proto::ErrorCode;
use rookery_store::testing::TestDb;
use serde_json::{Value, json};

/// A provisioner that writes each request to `requests.log` of its folder and
/// answers from files there: `answer.VERB` on standard output, `say.VERB` on
/// standard error and `code.VERB` as its exit code.
const FAKE: &str = r#"#!/bin/sh
verb="$1"
request=$(cat)
printf '%s %s\n' "$verb" "$request" >> requests.log
[ -f "say.$verb" ] && cat "say.$verb" >&2
[ -f "answer.$verb" ] && cat "answer.$verb"
exit "$(cat "code.$verb" 2>/dev/null || echo 0)"
"#;

const MANIFEST: &str = r#"{
  "apiVersion": "1",
  "name": "Lab",
  "contributes": {
    "provisioners": [
      {
        "id": "vm",
        "label": "Lab machine",
        "description": "A machine in the lab.",
        "command": "bin/provision",
        "protocol": 2,
        "verbs": ["list"],
        "arguments": [],
        "kinds": [
          {
            "id": "machine",
            "label": "Machine",
            "setup": "apply",
            "verbs": ["start", "stop", "logs"],
            "arguments": [
              {"name": "cpus", "label": "CPUs", "default": "2"},
              {"name": "image", "label": "Image", "default": "ubuntu/24.04"}
            ],
            "catalogue": {
              "sizes": [
                {"id": "small", "label": "Small", "values": {"cpus": "2"}},
                {"id": "large", "label": "Large", "default": true, "values": {"cpus": "8", "nonsense": "x"}}
              ],
              "images": [
                {"id": "debian-13", "label": "Debian 13", "values": {"image": "debian/13"}}
              ]
            }
          },
          {
            "id": "deployment",
            "label": "Deployment",
            "arguments": [{"name": "name"}]
          }
        ]
      }
    ]
  }
}"#;

struct Server {
    kernel: Kernel,
    db: TestDb,
    /// The installation's folders, with the plugins installed under `config`.
    dir: tempfile::TempDir,
}

impl Server {
    async fn start() -> Option<Server> {
        let db = TestDb::new().await?;
        let dir = tempfile::tempdir().unwrap();
        let paths = rookery_core::paths::Paths {
            config_dir: dir.path().join("config"),
            data_dir: dir.path().join("data"),
        };
        let checkout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        fs::create_dir_all(&paths.config_dir).unwrap();
        let mut methods = Methods::default();
        rookery_kernel::streams::register(&mut methods);
        rookery_facilities::register(&mut methods);
        let hosts = rookery_hosts::Registration::new();
        let options = rookery_jobs::Options {
            backoff: Duration::from_millis(50),
            ..Default::default()
        };
        rookery_jobs::register(&mut methods, hosts.handlers(), options);
        hosts.register(&mut methods);
        let kernel = Kernel::new(
            Config::new("http://127.0.0.1:7777"),
            db.store.clone(),
            methods,
        );
        kernel.set_facility(rookery_hosts::Setup {
            database_url: db.url.clone(),
            secret_key: Some(db.store.key().unwrap().clone()),
            paths: paths.clone(),
            provision: Default::default,
        });
        kernel.methods.started(&kernel);
        let serve = rookery_hosts::hosts(&kernel).ready().await.unwrap();
        let source = checkout.canonicalize().unwrap();
        tokio::task::spawn_blocking(move || {
            serve.paths.set("source", source.to_str().unwrap()).unwrap()
        })
        .await
        .unwrap();
        Some(Server { kernel, db, dir })
    }

    /// The folder of the lab package's target, which the fake command is in.
    fn package(&self) -> PathBuf {
        self.dir.path().join("config/plugins/example/lab/rookery")
    }

    /// Install the package in the plugins folder, locked as an install would
    /// leave it. `answers` are written beside the command: `answer.status`,
    /// `say.create` and so on.
    fn install(&self, answers: &[(&str, &str)]) {
        let package = self.package();
        fs::create_dir_all(package.join("bin")).unwrap();
        fs::write(
            self.dir
                .path()
                .join("config/plugins/example/lab/package.kdl"),
            "format 1\npackage {\n  group example\n  id lab\n  version \"1.0.0\"\n}\ntargets {\n  rookery {\n    requires {\n      rookery \">=0.6\"\n    }\n  }\n}\n",
        )
        .unwrap();
        fs::write(package.join("plugin.json"), MANIFEST).unwrap();
        let command = package.join("bin/provision");
        fs::write(&command, FAKE).unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        for (name, text) in answers {
            fs::write(package.join(name), text).unwrap();
        }
        fs::write(
            self.dir.path().join("config/plugins/eggs.lock"),
            format!(
                "format = 1\n\n[[package]]\nname = \"example/lab\"\nversion = \"1.0.0\"\ntargets = [\"rookery\"]\nfolder = \"{}\"\nsha256 = \"00\"\n",
                self.dir.path().join("config/plugins/example/lab").display()
            ),
        )
        .unwrap();
    }

    /// Forget the package, leaving a registered provisioner without its plugin.
    fn uninstall(&self) {
        let _ = fs::remove_file(self.dir.path().join("config/plugins/eggs.lock"));
    }

    async fn admin(&self, name: &str) -> Caller {
        let login = rookery_store::users::Login {
            issuer: "https://id.example.test".into(),
            subject: name.into(),
            name: name.into(),
            email: Some(format!("{}@example.test", name.to_lowercase())),
            admin: true,
        };
        let (user, _) = self.db.store.users().upsert_login(&login).await.unwrap();
        Caller::User {
            user: rookery_kernel::auth::wire_user(&user),
            session: None,
            token: None,
        }
    }

    async fn call(&self, caller: &Caller, method: &str, params: Value) -> Result<Value, RpcError> {
        let call = Call::new(self.kernel.clone(), caller.clone(), None);
        self.kernel.methods.dispatch(call, method, params).await
    }

    /// A call that has to answer, with what went wrong when it did not.
    async fn ok(&self, caller: &Caller, method: &str, params: Value) -> Value {
        self.call(caller, method, params)
            .await
            .unwrap_or_else(|error| panic!("{method}: {error}"))
    }

    /// The job once it has ended, and its log.
    async fn ended(&self, caller: &Caller, id: &str) -> (Value, Vec<String>) {
        for _ in 0..400 {
            let job = self
                .call(caller, "jobs.get", json!({"id": id}))
                .await
                .unwrap();
            if matches!(
                job["state"].as_str(),
                Some("succeeded" | "failed" | "cancelled")
            ) {
                let log = self
                    .call(caller, "jobs.output", json!({"id": id, "after": 0}))
                    .await
                    .unwrap();
                let lines = log["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|item| item["text"].as_str().unwrap().to_owned())
                    .collect();
                return (job, lines);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the job {id} did not end");
    }

    async fn close(self) {
        rookery_core::store::set_shared(None);
        rookery_core::runner::set_locks(None);
        self.kernel.shutdown.cancel();
        self.db.close().await;
    }
}

fn code(result: Result<Value, RpcError>) -> Option<ErrorCode> {
    result.err().map(|error| error.error_code())
}

/// The profile of a host a provisioner makes, which comes with an account
/// rookery reaches over SSH.
fn profile() -> Value {
    json!({
        "hostname": "studio",
        "access": {"user": "ops"},
        "builder": {},
        "ansible_vars": {"rookery_install_tailscale": false, "rookery_plugin_features": ["lab/thing"]},
    })
}

async fn the_catalogue_offers_what_plugins_declare_and_nothing_else(
    server: &Server,
    admin: &Caller,
) {
    // What Rookery's own plugins bring may be offered; the lab's is not before it is installed.
    let before = server.ok(admin, "setup.catalogue", json!({})).await;
    assert!(
        before["machines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|known| known["provisioner"] != "lab/vm"),
        "{before}"
    );

    server.install(&[(
        "answer.status",
        r#"{"state": "ready", "message": "Lab answers"}"#,
    )]);
    let found = server.ok(admin, "setup.catalogue", json!({})).await;
    let machines: Vec<&Value> = found["machines"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|known| known["provisioner"] == "lab/vm")
        .collect();
    // The deployment kind declares no choices, so it offers no machine.
    assert_eq!(machines.len(), 1, "{found}");
    let machine = machines[0];
    assert_eq!(machine["provisioner"], "lab/vm");
    assert_eq!(machine["kind"], "machine");
    assert_eq!(machine["label"], "Machine");
    let sizes = machine["catalogue"]["sizes"].as_array().unwrap();
    assert_eq!(sizes.len(), 2, "{machine}");
    assert_eq!(sizes[1]["id"], "large");
    assert_eq!(sizes[1]["default"], true);
    // A value of an argument the kind does not take is left out.
    assert_eq!(sizes[1]["values"], json!({"cpus": "8"}), "{machine}");
    assert_eq!(
        machine["catalogue"]["images"][0]["values"],
        json!({"image": "debian/13"})
    );
    // The deployment kind declares no choices, and a catalogue with no region leaves it out.
    assert!(machine["catalogue"].get("regions").is_none(), "{machine}");
}

async fn a_machine_is_made_in_the_call_that_saves_its_host(server: &Server, admin: &Caller) {
    server.install(&[
        (
            "answer.status",
            r#"{"state": "ready", "message": "Lab answers"}"#,
        ),
        ("say.create", "the lab refused it"),
        ("code.create", "1"),
    ]);
    server
        .ok(
            admin,
            "provisioners.save",
            json!({"name": "lab", "type": "lab/vm"}),
        )
        .await;
    // A machine is made for a saved host, so one not saved is refused.
    assert_eq!(
        code(
            server
                .call(
                    admin,
                    "resources.create",
                    json!({"provisioner": "lab", "kind": "machine", "host": "studio"})
                )
                .await
        ),
        Some(ErrorCode::NotFound)
    );

    // One call saves the host's profile and asks for its machine.
    let made = server
        .ok(
            admin,
            "resources.create",
            json!({
                "provisioner": "lab",
                "kind": "machine",
                "host": "studio",
                "profile": profile(),
                "arguments": {"cpus": "4", "image": "debian/13"},
            }),
        )
        .await;
    assert_eq!(made["resource"]["host"], "studio", "{made}");
    assert_eq!(made["resource"]["kind"], "machine");
    assert_eq!(made["resource"]["arguments"]["cpus"], "4");
    assert!(made["job"]["id"].is_string(), "{made}");
    // The host is there now, with what the call gave it.
    let saved = server
        .ok(admin, "hosts.profile", json!({"host": "studio"}))
        .await;
    let mut wanted = profile();
    // Making a machine sets the host's target to the cloud, as a machine is.
    wanted["target"] = json!("cloud");
    assert_eq!(saved["profile"], wanted, "{saved}");
    // Its machine was asked of the provisioner, with what the call gave it.
    let (job, log) = server
        .ended(admin, made["job"]["id"].as_str().unwrap())
        .await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(
        log.iter().any(|line| line.contains("the lab refused it")),
        "{log:?}"
    );
    // The machine the provisioner did not make is still rookery's record of one.
    let kept = server
        .ok(admin, "resources.list", json!({"host": "studio"}))
        .await;
    assert_eq!(kept.as_array().unwrap().len(), 1, "{kept}");
}

async fn a_call_that_is_refused_leaves_no_host_behind(server: &Server, admin: &Caller) {
    server.install(&[(
        "answer.status",
        r#"{"state": "ready", "message": "Lab answers"}"#,
    )]);
    // A kind that is not a machine: a machine is refused before anything runs.
    assert_eq!(
        code(
            server
                .call(
                    admin,
                    "resources.create",
                    json!({
                        "provisioner": "lab/vm",
                        "kind": "machine",
                        "host": "forge",
                        "profile": json!({"access": {"user": "ops"}, "builder": {}, "ansible_vars": {"rookery_install_tailscale": false}}),
                        "arguments": {"no_such_argument": "x"},
                    })
                )
                .await
        ),
        Some(ErrorCode::Invalid)
    );
    // The profile the refused call saved is gone: the same call can be made again.
    assert_eq!(
        code(
            server
                .call(admin, "hosts.profile", json!({"host": "forge"}))
                .await
        ),
        Some(ErrorCode::NotFound)
    );
}

async fn the_list_says_how_each_provisioner_is_without_a_call_per_name(
    server: &Server,
    admin: &Caller,
) {
    server.install(&[(
        "answer.status",
        r#"{"state": "ready", "message": "Lab answers"}"#,
    )]);
    server
        .ok(
            admin,
            "provisioners.save",
            json!({"name": "lab", "type": "lab/vm"}),
        )
        .await;
    let listed = server.ok(admin, "provisioners.list", json!({})).await;
    let known = listed.as_array().unwrap();
    assert_eq!(known.len(), 1, "{listed}");
    assert_eq!(known[0]["state"], "ready", "{listed}");
    assert_eq!(known[0]["message"], "Lab answers");

    // One whose plugin is gone says why it cannot be used, and is not asked.
    server.uninstall();
    let listed = server.ok(admin, "provisioners.list", json!({})).await;
    let known = listed.as_array().unwrap();
    assert_eq!(known[0]["state"], "unusable", "{listed}");
    assert!(
        known[0]["message"]
            .as_str()
            .unwrap()
            .contains("No plugin brings lab/vm any more"),
        "{listed}"
    );
}

async fn removing_a_provisioner_says_what_holds_it(server: &Server, admin: &Caller) {
    server.install(&[(
        "answer.status",
        r#"{"state": "ready", "message": "Lab answers"}"#,
    )]);
    server
        .ok(
            admin,
            "provisioners.save",
            json!({"name": "lab", "type": "lab/vm"}),
        )
        .await;
    // A deployment is no host's machine, so it needs only a name.
    let made = server
        .ok(
            admin,
            "resources.create",
            json!({"provisioner": "lab", "kind": "deployment", "name": "web"}),
        )
        .await;
    let id = made["resource"]["id"].as_str().unwrap().to_owned();
    let refused = server
        .call(admin, "provisioners.remove", json!({"name": "lab"}))
        .await
        .unwrap_err();
    assert_eq!(refused.error_code(), ErrorCode::Conflict);
    let held = refused
        .details()
        .and_then(|details| details["resources"].as_array().cloned())
        .unwrap_or_default();
    let deployment = held
        .iter()
        .find(|known| known["id"] == id.as_str())
        .unwrap_or_else(|| panic!("{refused:?}"));
    assert_eq!(deployment["kind"], "deployment");
}

async fn a_setting_a_browser_cannot_change_says_what_to_run(server: &Server, admin: &Caller) {
    let listed = server
        .ok(admin, "installation.settings.list", json!({}))
        .await;
    let settings = listed.as_array().unwrap();
    let read_only: Vec<&Value> = settings
        .iter()
        .filter(|setting| setting["open"] == false)
        .collect();
    assert!(!read_only.is_empty(), "{listed}");
    for setting in read_only {
        let key = setting["key"].as_str().unwrap();
        assert_eq!(
            setting["command"],
            format!("rookery config set {key} VALUE"),
            "{setting}"
        );
    }
    let open = settings
        .iter()
        .find(|setting| setting["open"] == true)
        .unwrap();
    assert!(open.get("command").is_none(), "{open}");
}

/// One server for every part: rookery-core's store is one per process, so
/// tests that each start one would take it from each other.
#[tokio::test(flavor = "multi_thread")]
async fn provisioning_catalogue_list_removal_and_settings() {
    let Some(server) = Server::start().await else {
        return;
    };
    let admin = server.admin("ada").await;
    the_catalogue_offers_what_plugins_declare_and_nothing_else(&server, &admin).await;
    a_machine_is_made_in_the_call_that_saves_its_host(&server, &admin).await;
    a_call_that_is_refused_leaves_no_host_behind(&server, &admin).await;
    removing_a_provisioner_says_what_holds_it(&server, &admin).await;
    a_setting_a_browser_cannot_change_says_what_to_run(&server, &admin).await;
    // Last, as it takes the plugin away.
    the_list_says_how_each_provisioner_is_without_a_call_per_name(&server, &admin).await;
    server.close().await;
}
