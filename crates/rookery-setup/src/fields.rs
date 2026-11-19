//! The settings of a host's profile, and the steps and actions around them.

/// What a host can be installed on, as its profile's `target` says.
pub const TARGETS: &[&str] = &["laptop", "cloud"];
pub const DISK_MODES: &[&str] = &[
    "auto", "serial", "model", "path", "largest", "smallest", "ssd", "hdd",
];
pub const DISK_TYPES: &[&str] = &["any", "ssd", "hdd"];
/// The ways to log in that are not a key of the ring; `key:NAME` is offered
/// for each of those.
pub const LOGINS: &[&str] = &["rookery", "password", "ssh"];
/// The types a host can be meant to be, the whole profile first.
pub const TYPES: &[&str] = &["workstation", "node", "managed", "ssh"];
/// How a host no provisioner made is stopped: not by rookery, or by its
/// controller shutting it down.
pub const POWER_STOPS: &[&str] = &["none", "controller"];

/// The parts of a host's set-up, in the order the flow asks them. Each
/// depends only on the ones before it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step {
    /// How the host is installed and reached: what has to be chosen.
    Install,
    Features,
    /// What has a default that follows from the parts before.
    Settings,
    /// The last few things, every problem, and the buttons.
    Run,
    Manage,
}

impl Step {
    /// The parts of the set-up flow.
    pub const SETUP: [Step; 4] = [Step::Install, Step::Features, Step::Settings, Step::Run];

    pub fn label(self) -> &'static str {
        match self {
            Step::Install => "Install",
            Step::Features => "Features",
            Step::Settings => "Settings",
            Step::Run => "Run",
            Step::Manage => "Overview",
        }
    }

    /// The part `delta` places along, wrapping around at the ends.
    pub fn offset(self, delta: isize) -> Step {
        if self == Step::Manage {
            return self;
        }
        let index = (self as isize + delta).rem_euclid(Self::SETUP.len() as isize);
        Self::SETUP[index as usize]
    }
}

/// How much the agent account may do with sudo.
pub const PRIVILEGES: &[&str] = &["full", "limited", "none"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Save,
    Release,
    UsbSeed,
    Iso,
    CloudSeed,
    Machine,
    Apply,
    Docker,
    Ssh,
    /// An existing host rookery only logs in to: a way that chooses
    /// [`Action::Ssh`] for a host of the type ssh.
    Reach,
    Manage,
}

impl Action {
    /// The ways a rookery server installs a host itself, as
    /// `rookery host create` offers them. The USB installer is built on the
    /// machine the server runs on.
    pub const SERVED: [Action; 5] = [
        Action::Machine,
        Action::Iso,
        Action::CloudSeed,
        Action::Ssh,
        Action::Reach,
    ];

    /// The way's name on the command line, as `--method` takes it.
    pub fn key(self) -> &'static str {
        match self {
            Action::Save => "save",
            Action::Release => "release",
            Action::UsbSeed => "usb-seed",
            Action::Iso => "iso",
            Action::CloudSeed => "cloud-seed",
            Action::Machine => "provisioner",
            Action::Apply => "inventory",
            Action::Docker => "container",
            Action::Ssh => "ssh",
            Action::Reach => "ssh-only",
            Action::Manage => "manage",
        }
    }

    /// The way of [`Action::SERVED`] a name on the command line means.
    pub fn by_key(key: &str) -> Option<Action> {
        Action::SERVED
            .into_iter()
            .find(|action| action.key() == key)
    }

    /// The types a host installed this way can have. Only over SSH can rookery
    /// install less than a workstation: an installer, a seed or a provisioner
    /// installs the whole profile from a blank machine.
    pub fn types(self) -> &'static [rookery_api::HostType] {
        use rookery_api::HostType::*;
        match self {
            Action::Reach => &[Ssh],
            Action::Ssh | Action::Manage => &[Managed, Node, Workstation],
            _ => &[Workstation],
        }
    }

    /// Whether this way is offered to a host of `host_type`:
    /// the ways its type allows, and the two over SSH, which move a host
    /// between the type ssh and the types rookery installs.
    pub fn offered(self, host_type: rookery_api::HostType) -> bool {
        matches!(self, Action::Ssh | Action::Reach) || self.types().contains(&host_type)
    }

    /// The action a way sets, with the type it gives the
    /// profile when it implies one.
    pub fn choose(self, config: &mut serde_json::Value) -> Action {
        use rookery_api::HostType;
        let host_type = rookery_api::profile::host_type(config);
        match self {
            Action::Reach => {
                rookery_api::profile::set_host_type(config, HostType::Ssh);
                Action::Ssh
            }
            // Installing over SSH installs something: the whole profile, unless the controller is chosen.
            Action::Ssh if host_type == HostType::Ssh => {
                rookery_api::profile::set_host_type(config, HostType::Workstation);
                Action::Ssh
            }
            action if !action.types().contains(&host_type) => {
                rookery_api::profile::set_host_type(config, HostType::Workstation);
                action
            }
            action => action,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Save => "Save configuration",
            Action::Release => "Build release",
            Action::UsbSeed => "Render USB seed",
            Action::Iso => "USB installer",
            Action::CloudSeed => "Cloud-init seed",
            Action::Machine => "From a provisioner",
            Action::Apply => "This machine or an inventory",
            Action::Docker => "Container image",
            Action::Ssh => "Existing host over SSH",
            Action::Reach => "Existing host, SSH only",
            Action::Manage => "Manage an installed host",
        }
    }

    pub fn help(self) -> &'static str {
        match self {
            Action::Save => "Save your selections as a private JSON profile.",
            Action::Release => "Package the provisioning source for distribution.",
            Action::UsbSeed => {
                "Generate the NoCloud files and provisioning bundle for an unattended installer."
            }
            Action::Iso => {
                "For a blank machine: build an installer image from the Ubuntu Server ISO that installs and sets up the host without questions, and write it to a USB drive. Without an ISO the current one is downloaded and checked first. It is built on the machine rookery serve runs on."
            }
            Action::CloudSeed => {
                "Generate cloud-init user data. Upload the generated release archive to the configured URL."
            }
            Action::Machine => {
                "Have a provisioner make the host, such as a container or a virtual machine on a server, and set it up. Choose the provisioner below; plugins bring them. Its size is asked for with the settings, and it is created at the end."
            }
            Action::Apply => {
                "Provision a dedicated Ubuntu host using Ansible. Blank inventory applies to this machine."
            }
            Action::Docker => {
                "Build the desktop image using Docker Compose. Start it afterwards with docker compose."
            }
            Action::Manage => {
                "Look after a host that is already set up, over SSH: show its status and access links, publish the desktop on the tailnet, run the smoke test, install updates or reboot. Run the action by name."
            }
            Action::Ssh => {
                "Install on an existing Ubuntu host over SSH. Enter a hostname, IP address or SSH config alias, and how rookery logs in: its own key by default. A host that does not take that key yet gets it with rookery key trust HOST and a password used once."
            }
            Action::Reach => {
                "Add a host rookery only logs in to over SSH, such as a server that stays as it is: nothing is installed on it. Enter a hostname, IP address or SSH config alias, and how rookery logs in. Run saves it; a host that does not take rookery's key yet gets it with rookery key trust HOST. Its page can install the controller later, for sessions, actions and files."
            }
        }
    }

    /// Actions that write an installer for a blank machine.
    pub fn installer(self) -> bool {
        matches!(self, Action::UsbSeed | Action::Iso)
    }

    /// Actions that make a machine nobody has logged in to, from the profile alone.
    pub fn seeds(self) -> bool {
        matches!(self, Action::CloudSeed | Action::Machine)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Text,
    Secret,
    Path,
    Toggle,
    Choice(&'static [&'static str]),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Heading(&'static str),
    Under(Field),
}

pub struct Spec {
    pub label: &'static str,
    /// JSON pointer of the value inside the profile.
    pub pointer: &'static str,
    pub kind: Kind,
    pub help: &'static str,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Field {
    Target,
    Hostname,
    Keyboard,
    Geometry,
    ReleaseUrl,
    RecoveryCidr,
    ReleaseStore,
    ReleasePrivate,
    DiskMode,
    DiskSerial,
    DiskModel,
    DiskPath,
    DiskType,
    Wifi,
    WifiInterface,
    WifiSsid,
    WifiPassword,
    OperatorUser,
    OperatorKey,
    OperatorHash,
    AgentUser,
    AgentKey,
    AgentHash,
    AgentSudo,
    AgentPrivileges,
    ToolsWeb,
    ToolsNative,
    ToolsJvm,
    ToolsDotnet,
    ToolsScripting,
    ToolsScientific,
    ToolsChrome,
    Playwright,
    SoftwareRendering,
    HardenSsh,
    Cockpit,
    Docker,
    Tailscale,
    Iso,
    IsoSha,
    UsbDevice,
    Inventory,
    AskBecomePass,
    SshHost,
    SshUser,
    SshPort,
    Login,
    Type,
    PowerStop,
}

impl Field {
    /// Every field, in the order of the set-up tabs.
    pub const ALL: [Field; 49] = {
        use Field::*;
        [
            Type,
            Target,
            SshHost,
            SshUser,
            SshPort,
            Login,
            AskBecomePass,
            PowerStop,
            Hostname,
            Tailscale,
            ToolsWeb,
            ToolsNative,
            ToolsJvm,
            ToolsDotnet,
            ToolsScripting,
            ToolsScientific,
            ToolsChrome,
            Playwright,
            SoftwareRendering,
            Cockpit,
            Docker,
            OperatorUser,
            OperatorKey,
            OperatorHash,
            AgentUser,
            AgentHash,
            AgentKey,
            AgentPrivileges,
            AgentSudo,
            HardenSsh,
            RecoveryCidr,
            Geometry,
            DiskMode,
            DiskSerial,
            DiskModel,
            DiskPath,
            DiskType,
            Keyboard,
            Wifi,
            WifiSsid,
            WifiPassword,
            WifiInterface,
            ReleaseStore,
            ReleaseUrl,
            ReleasePrivate,
            Iso,
            IsoSha,
            UsbDevice,
            Inventory,
        ]
    };

    /// The field's name on the command line, as `--set KEY=VALUE` takes it:
    /// its place in the profile, without the parts every field of a kind
    /// shares.
    pub fn key(self) -> String {
        let pointer = self.spec().pointer.trim_start_matches('/');
        for (part, instead) in [
            ("access/", ""),
            ("builder/", ""),
            ("ansible_vars/rookery_", ""),
            ("wifi/", "wifi_"),
            ("power/stop/by", "power_stop"),
        ] {
            if let Some(rest) = pointer.strip_prefix(part) {
                return format!("{instead}{rest}");
            }
        }
        pointer.to_owned()
    }

    /// The field a name on the command line means.
    pub fn by_key(key: &str) -> Option<Field> {
        Field::ALL.into_iter().find(|field| field.key() == key)
    }

    /// Whether `rookery host create` asks for the field when it is shown and
    /// not given: a choice most hosts make. Others are asked for only when
    /// a problem names them.
    pub fn asked(self) -> bool {
        use Field::*;
        matches!(
            self,
            Target | Type | SshHost | SshUser | OperatorUser | AgentUser
        )
    }

    pub fn spec(self) -> Spec {
        use Field::*;
        use Kind::*;
        let (label, pointer, kind, help) = match self {
            Target => (
                "Target",
                "/target",
                Choice(TARGETS),
                "What the host is installed on: laptop or cloud, with their provisioning defaults. Only laptop adds a physical login and power settings. Cloud leaves SSH open to any address when no recovery SSH CIDR is set; for a QEMU guest, set that to 10.0.2.2/32.",
            ),
            Hostname => (
                "Hostname",
                "/hostname",
                Text,
                "Hostname assigned to the installed target. Lowercase letters, numbers and hyphens, starting with a letter. Left empty, the host's name is used when it can be one.",
            ),
            Keyboard => (
                "Keyboard layout",
                "/keyboard",
                Text,
                "Keyboard layout used by the installer, such as de or us.",
            ),
            Geometry => (
                "Desktop size",
                "/ansible_vars/rookery_geometry",
                Text,
                "Resolution of the virtual desktop, for example 1600x900. Blank keeps the default.",
            ),
            ReleaseUrl => (
                "Release HTTPS URL",
                "/release_url",
                Text,
                "HTTPS URL where you will publish the generated release archive.",
            ),
            ReleaseStore => (
                "Publish release to",
                "/release_store",
                Text,
                "Where the new machine gets the release archive from. It is a file server or a raw repository of the registries; rookery uploads the archive there when the seed is made. Or upload it yourself and enter its address below.",
            ),
            ReleasePrivate => (
                "Download with login",
                "/release_private",
                Toggle,
                "Off: the machine downloads the archive without a login, so the store has to allow anonymous reads. On: the seed carries the login of that registry in a root-only file that is deleted after the download. Anyone who can read the seed can read that login.",
            ),
            RecoveryCidr => (
                "Recovery SSH CIDR",
                "/ansible_vars/rookery_bootstrap_ssh_cidr",
                Text,
                "Network allowed to use SSH from outside the tailnet, such as your admin IP with /32. Left empty, a cloud host accepts SSH from any address, and other hosts only from the tailnet and the session used for installing.",
            ),
            DiskMode => (
                "Disk selection",
                "/target_disk_mode",
                Choice(DISK_MODES),
                "How the installer picks the disk it erases. Auto requires exactly one eligible internal disk. Every mode stops when the match is ambiguous.",
            ),
            DiskSerial => (
                "Target disk serial",
                "/target_disk_serial",
                Text,
                "Exact udev ID_SERIAL of the target disk. The unattended installer erases the matching disk.",
            ),
            DiskModel => (
                "Disk model pattern",
                "/target_disk_model",
                Text,
                "Match udev ID_MODEL, with glob patterns such as Samsung*. Multiple matches stop installation.",
            ),
            DiskPath => (
                "Disk path pattern",
                "/target_disk_path",
                Path,
                "Match a /dev/ disk path or pattern. Exact /dev/disk/by-id symlinks are resolved during installation.",
            ),
            DiskType => (
                "Disk type filter",
                "/target_disk_ssd",
                Choice(DISK_TYPES),
                "Optional filter for model, path, auto or size selection.",
            ),
            Wifi => (
                "Wi-Fi",
                "/wifi_enabled",
                Toggle,
                "Configure Wi-Fi in the unattended installer. Wired DHCP stays available either way.",
            ),
            WifiInterface => (
                "Wi-Fi interface",
                "/wifi/interface",
                Text,
                "Leave blank to use the wireless interface found on the target during installation. Set a name such as wlp1s0 only to force one.",
            ),
            WifiSsid => (
                "Wi-Fi SSID",
                "/wifi/ssid",
                Text,
                "Wireless network name. Networks saved on this machine are listed in Suggestions, and choosing one with its saved password fills that in too.",
            ),
            WifiPassword => (
                "Wi-Fi password",
                "/wifi/password",
                Secret,
                "Wireless network password, saved in the private profile. When this machine knows the network, the flow says whether the password matches.",
            ),
            OperatorUser => (
                "Operator username",
                "/operator_username",
                Text,
                "Account for human login and management. Ubuntu reserves names such as operator, admin and root.",
            ),
            OperatorKey => (
                "SSH key",
                "/operator_ssh_public_keys",
                Text,
                "An OpenSSH public key the operator account takes. The account can have several: one for each person or machine that logs in.",
            ),
            OperatorHash => (
                "Operator password hash",
                "/operator_password_hash",
                Secret,
                "SHA-512 crypt or yescrypt hash of the operator password. Generate a password and hash, and rookery host credentials HOST shows the password later, or enter a password to hash.",
            ),
            AgentUser => (
                "Agent username",
                "/agent_username",
                Text,
                "Account used by coding agents and the virtual desktop. Home is /home/USERNAME.",
            ),
            AgentKey => (
                "SSH key",
                "/agent_ssh_public_keys",
                Text,
                "An OpenSSH public key for SSH access to the agent account. The account can have several, or none.",
            ),
            AgentHash => (
                "Agent password hash",
                "/agent_password_hash",
                Secret,
                "Optional local password for the agent. Generate a password and hash, and rookery host credentials HOST shows the password later, or enter a password to hash. Empty locks password login.",
            ),
            AgentPrivileges => (
                "Agent privileges",
                "/agent_privileges",
                Choice(PRIVILEGES),
                "What the agent may do with sudo. full: everything, for machines you would throw away. limited: install and update packages with apt, and restart its own desktop and services; it cannot touch SSH, Tailscale, the firewall, the SSH user or rookery's controller. none: nothing. It guards against accidents, not intent: package scripts run as root.",
            ),
            AgentSudo => (
                "Agent passwordless sudo",
                "/agent_passwordless_sudo",
                Toggle,
                "Allow the agent to use sudo without a password. Turning this off requires an agent password.",
            ),
            ToolsWeb => (
                "JavaScript runtimes",
                "/ansible_vars/rookery_tools_web",
                Toggle,
                "Install Bun/bunx, Deno and pnpm. Base Node and npm support the coding agents.",
            ),
            ToolsNative => (
                "Native development",
                "/ansible_vars/rookery_tools_native",
                Toggle,
                "Install Go, Clang and Rust with Cargo/rustup. Base GCC, CMake and Ninja remain available.",
            ),
            ToolsJvm => (
                "JVM development",
                "/ansible_vars/rookery_tools_jvm",
                Toggle,
                "Install OpenJDK 21, Maven, Gradle, Kotlin and Scala/sbt.",
            ),
            ToolsDotnet => (
                ".NET development",
                "/ansible_vars/rookery_tools_dotnet",
                Toggle,
                "Install the .NET SDK and native runtime dependencies.",
            ),
            ToolsScripting => (
                "Scripting languages",
                "/ansible_vars/rookery_tools_scripting",
                Toggle,
                "Install Ruby/Bundler, PHP/Composer and Erlang/Elixir.",
            ),
            ToolsScientific => (
                "Scientific computing",
                "/ansible_vars/rookery_tools_scientific",
                Toggle,
                "Install R for scientific computing.",
            ),
            ToolsChrome => (
                "Google Chrome",
                "/ansible_vars/rookery_tools_chrome",
                Toggle,
                "Install native Google Chrome from its vendor repository.",
            ),
            Playwright => (
                "Playwright",
                "/ansible_vars/rookery_install_playwright",
                Toggle,
                "Install Playwright and its matching Chromium build.",
            ),
            SoftwareRendering => (
                "Software graphics",
                "/ansible_vars/rookery_software_rendering",
                Toggle,
                "Use Mesa CPU rendering for the virtual desktop and headless jobs. Turn off on a machine with a GPU to render on it instead, for example for the Unity editor.",
            ),
            HardenSsh => (
                "SSH hardening",
                "/ansible_vars/rookery_harden_ssh",
                Toggle,
                "Accept SSH keys only and refuse root logins. Applied only when the operator has an SSH public key or the account used for installing has one, so it cannot lock you out. Console, Cockpit and sudo still use passwords.",
            ),
            Cockpit => (
                "Cockpit web console",
                "/ansible_vars/rookery_install_cockpit",
                Toggle,
                "Install the Cockpit web console for logs, services, storage, updates, files and a terminal. It listens on port 9090 and is reachable over the tailnet or an SSH tunnel. Log in as the operator with its password.",
            ),
            Docker => (
                "Docker on host",
                "/ansible_vars/rookery_install_docker",
                Toggle,
                "Install Docker, Compose and Buildx on the target host.",
            ),
            Tailscale => (
                "Tailscale",
                "/ansible_vars/rookery_install_tailscale",
                Toggle,
                "Install Tailscale and join the tailnet with a one-use key rookery makes through the control server's API, from the secret tailnet_api_key and the tailnet_ settings. A host on the tailnet already is left on it. Off, rookery reaches the host over SSH at its address, and the desktop and T3 through an SSH tunnel.",
            ),
            Iso => (
                "Source ISO",
                "/builder/iso",
                Path,
                "Ubuntu Server 26.04 amd64 installer ISO to build from, a path on the machine rookery serve runs on. Left empty, the build downloads the current one and checks it against Ubuntu's published SHA256SUMS.",
            ),
            IsoSha => (
                "ISO SHA256",
                "/builder/iso_sha256",
                Text,
                "SHA-256 the source ISO must have, which the build checks. Take it from Ubuntu's published SHA256SUMS.",
            ),
            Inventory => (
                "Ansible inventory",
                "/builder/inventory",
                Path,
                "Path to an Ansible inventory file. Blank provisions this machine.",
            ),
            AskBecomePass => (
                "Ask for sudo password",
                "/access/sudo",
                Toggle,
                "Ask for the SSH user's sudo password, for a first apply as an account of the machine's own whose sudo asks for one. The operator never needs it: once the host is applied as its operator, rookery stops asking. The password is asked for when it is needed and kept until rookery is closed, or kept as the secret sudo/HOST if you store it there.",
            ),
            SshHost => (
                "SSH address",
                "/access/address",
                Text,
                "The host's name or IP address, or a Host alias from ~/.ssh/config.",
            ),
            SshUser => (
                "SSH user",
                "/access/user",
                Text,
                "The account rookery logs in as. Leave blank to use your SSH configuration.",
            ),
            SshPort => (
                "SSH port",
                "/access/port",
                Text,
                "SSH port. Leave blank for the SSH configuration default.",
            ),
            Login => (
                "Log in with",
                "/access/auth",
                Choice(LOGINS),
                "How rookery logs in. rookery: the installation's own key, which hosts it sets up take. key:NAME: another key of rookery's key ring (rookery key list names them). password: a password, kept as the secret ssh/HOST or asked for. ssh: your own SSH configuration and agent, with rookery's key offered after them. A host that does not take its key yet gets it with: rookery key trust HOST.",
            ),
            Type => (
                "Type",
                "/type",
                Choice(TYPES),
                "What rookery does with the host. workstation: the whole profile, with the desktop, the agents and plugins. node: a server for machines, applied without a desktop or accounts, with the plugin features that install Incus or Docker and register their provisioners. managed: rookeryd and box alone, for sessions, actions and files on a machine that stays as it is. ssh: nothing is installed; rookery logs in over SSH. The type can change later: install the controller, or set workstation and apply.",
            ),
            PowerStop => (
                "Stopped by",
                "/power/stop/by",
                Choice(POWER_STOPS),
                "How rookery stops this host when no provisioner made it. controller: the host's controller shuts the machine down, which ends its sessions; rookery cannot start it again, so it is started by hand or by whatever powers it. none: rookery does not stop it. A host a provisioner made is started and stopped through the provisioner.",
            ),
            UsbDevice => (
                "Then write to USB drive",
                "/builder/usb_device",
                Text,
                "The USB drive the finished ISO is written to, such as /dev/sdb, on the machine rookery serve runs on. It is erased. `rookery host iso` asks for it again, with your sudo password, when the installer is built.",
            ),
        };
        Spec {
            label,
            pointer,
            kind,
            help,
        }
    }

    /// Fields shown as a tree: under a heading, or under the field that reveals them.
    pub fn group(self) -> Option<Group> {
        use Field::*;
        Some(match self {
            OperatorUser | OperatorKey | OperatorHash => Group::Heading("Operator Account"),
            AgentUser | AgentKey | AgentHash | AgentSudo | AgentPrivileges => {
                Group::Heading("Agent Account")
            }
            ToolsWeb | ToolsNative | ToolsJvm | ToolsDotnet | ToolsScripting | ToolsScientific
            | ToolsChrome => Group::Heading("Development Tools"),
            DiskSerial | DiskModel | DiskPath | DiskType => Group::Under(DiskMode),
            WifiInterface | WifiSsid | WifiPassword => Group::Under(Wifi),
            ReleaseUrl | ReleasePrivate => Group::Under(ReleaseStore),
            IsoSha | UsbDevice => Group::Under(Iso),
            SshUser | SshPort | Login => Group::Under(SshHost),
            _ => return None,
        })
    }

    pub fn is_password_hash(self) -> bool {
        matches!(self, Field::OperatorHash | Field::AgentHash)
    }

    /// Fields that use the Wi-Fi logins saved on this machine.
    pub fn uses_saved_wifi(self) -> bool {
        matches!(self, Field::Wifi | Field::WifiSsid | Field::WifiPassword)
    }

    /// Fields whose value can be removed with x.
    pub fn can_clear(self) -> bool {
        matches!(self.spec().kind, Kind::Text | Kind::Secret | Kind::Path)
    }

    /// Fields with a Generate action.
    pub fn can_generate(self) -> bool {
        matches!(self, Field::Hostname) || self.is_password_hash()
    }
}
