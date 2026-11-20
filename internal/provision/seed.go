package provision

import (
	"fmt"
	"strings"

	"gopkg.in/yaml.v3"

	"github.com/aiviarium/rookery/internal/access"
)

// DefaultOperator is the account a machine's seed makes, unless the profile
// names another.
const DefaultOperator = "ops"

// Seed is what a machine needs to come up reachable, and nothing more: the
// operator account with the key ring's public key and passwordless sudo, and
// Tailscale joined with a one-use auth key. Everything else is the apply that
// follows the machine's creation.
type Seed struct {
	// Host is the host's name, which the machine's hostname and the tailnet
	// device take.
	Host string

	// Operator is the account's name. Empty is DefaultOperator.
	Operator string

	// PublicKey is the key ring's public key, in authorized_keys form: one line,
	// such as "ssh-ed25519 AAAA… rookery".
	PublicKey string

	// TailnetKey is a one-use, pre-authorised auth key the machine joins the
	// tailnet with. Empty leaves Tailscale out, for a deployment that has none.
	TailnetKey string
}

// Validate refuses a seed that would not bring a machine up reachable.
func (s Seed) Validate() error {
	switch {
	case !validHostName(s.Host):
		return fmt.Errorf("%w: a seed needs a valid host name", access.ErrInvalid)
	case strings.TrimSpace(s.PublicKey) == "":
		return fmt.Errorf("%w: a seed carries the key ring's public key", access.ErrInvalid)
	case strings.ContainsAny(s.PublicKey, "\n\r"):
		return fmt.Errorf("%w: the public key is one line", access.ErrInvalid)
	case strings.ContainsAny(s.TailnetKey, " \t\r\n"):
		return fmt.Errorf("%w: a tailnet auth key has no space", access.ErrInvalid)
	}
	if name := s.operator(); !validAccount(name) {
		return fmt.Errorf("%w: %q is not an account name", access.ErrInvalid, name)
	}
	return nil
}

// operator is the account the seed makes.
func (s Seed) operator() string {
	if s.Operator == "" {
		return DefaultOperator
	}
	return s.Operator
}

// RenderSeed is the cloud-init user data a machine starts with.
//
// It is YAML for cloud-init's NoCloud source, which every machine image Rookery
// offers reads: the file is the seed's user-data, and the image's meta-data is
// the host's name.
func RenderSeed(seed Seed) (string, error) {
	if err := seed.Validate(); err != nil {
		return "", err
	}

	document := cloudConfig{
		Hostname:        seed.Host,
		PreserveHost:    false,
		SSHPasswordAuth: false,
		Users: []cloudUser{{
			Name:       seed.operator(),
			Gecos:      "Rookery operator",
			Groups:     []string{"adm", "sudo"},
			Sudo:       "ALL=(ALL) NOPASSWD:ALL",
			Shell:      "/bin/bash",
			LockPasswd: true,
			SSHAuthorizedKeys: []string{
				seed.PublicKey,
			},
		}},
		WriteFiles: []cloudFile{{
			Path:        "/etc/cloud/cloud.cfg.d/99-rookery.cfg",
			Permissions: "0644",
			Content:     "datasource_list: [NoCloud, None]\n",
		}},
	}
	if seed.TailnetKey != "" {
		const keyPath = "/run/rookery-tailnet-key"
		document.WriteFiles = append(document.WriteFiles, cloudFile{
			Path: keyPath, Permissions: "0600", Content: seed.TailnetKey,
		})
		document.RunCmd = [][]string{
			{"sh", "-c", "curl -fsSL https://tailscale.com/install.sh | sh"},
			{"tailscale", "up", "--auth-key=file:" + keyPath, "--hostname=" + seed.Host},
			{"rm", "-f", keyPath},
		}
	}

	encoded, err := yaml.Marshal(document)
	if err != nil {
		return "", fmt.Errorf("provision: rendering the seed: %w", err)
	}
	return cloudConfigHeader + string(encoded), nil
}

// cloudConfigHeader is what makes a document cloud-init user data.
const cloudConfigHeader = "#cloud-config\n"

// cloudConfig is the part of cloud-init's user data Rookery writes.
type cloudConfig struct {
	Hostname        string      `yaml:"hostname"`
	PreserveHost    bool        `yaml:"preserve_hostname"`
	SSHPasswordAuth bool        `yaml:"ssh_pwauth"`
	Users           []cloudUser `yaml:"users"`
	WriteFiles      []cloudFile `yaml:"write_files"`
	RunCmd          [][]string  `yaml:"runcmd,omitempty"`
}

// cloudUser is one account the seed makes.
type cloudUser struct {
	Name              string   `yaml:"name"`
	Gecos             string   `yaml:"gecos"`
	Groups            []string `yaml:"groups"`
	Sudo              string   `yaml:"sudo"`
	Shell             string   `yaml:"shell"`
	LockPasswd        bool     `yaml:"lock_passwd"`
	SSHAuthorizedKeys []string `yaml:"ssh_authorized_keys"`
}

// cloudFile is one file the seed writes.
type cloudFile struct {
	Path        string `yaml:"path"`
	Permissions string `yaml:"permissions"`
	Content     string `yaml:"content"`
}

// validAccount reports whether a name can be an account's on a Linux host.
func validAccount(name string) bool {
	if name == "" || len(name) > 32 || name[0] == '-' {
		return false
	}
	for _, c := range name {
		switch {
		case c >= 'a' && c <= 'z', c >= '0' && c <= '9', c == '_', c == '-':
		default:
			return false
		}
	}
	return true
}
