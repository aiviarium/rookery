// Package apply runs one Ansible apply for a host.
//
// It makes a work directory under the server's state directory, writes the
// private key, the host's known key, the inventory, the extracted roles, the
// generated playbook, ansible.cfg and vars.json into it, and runs
// ansible-playbook there with every output line masked and sent to a sink. A
// cancelled context kills the run's process group, and the work directory is
// removed either way.
//
// What follows a successful run — storing the applied digest, reading
// AgentService.Version and setting the host's type from what it now has — is the
// caller's: it needs the store and the host's link, which this package does not
// know about. Apply returns the digest it applied for the caller to store.
package apply

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/aiviarium/rookery/ansible"
	"github.com/aiviarium/rookery/internal/hostaction"
	"github.com/aiviarium/rookery/internal/profile"
)

// ErrInvalid is the sentinel every refusal of a request wraps.
var ErrInvalid = errors.New("apply: invalid request")

// Binaries are the host programs an apply installs: rookeryd and box, built for
// the host's architecture.
type Binaries struct {
	Rookeryd string
	Box      string
}

// Registry is one registry login the host's root gets: a container registry and
// the account for it.
type Registry struct {
	Address  string `json:"address"`
	Username string `json:"username"`
	Password string `json:"password"`
}

// ActionArg is one argument of an action, as /etc/rookery/actions.json names it.
type ActionArg = hostaction.Argument

// Action is one action offered by a host feature.
type Action struct {
	Name     string      `json:"name"`
	Label    string      `json:"label"`
	Group    string      `json:"group,omitempty"`
	User     string      `json:"user"`
	OnHost   bool        `json:"-"`
	Terminal bool        `json:"terminal,omitempty"`
	Run      string      `json:"command"`
	Args     []ActionArg `json:"arguments,omitempty"`
}

// Host is what an apply knows about a host that its profile does not: its name,
// and where it is on the tailnet.
type Host struct {
	// Name is the host's name in Rookery, which is also its inventory name and
	// its name on the tailnet.
	Name string
	// TailnetAddress is its address on the tailnet, empty when it is not on one.
	// The inventory reaches the host there, through the server.
	TailnetAddress string
}

// Feature is one feature of an installed plugin that the profile applies.
type Feature struct {
	// Plugin and Feature name it; its role is <plugin>__<feature>.
	Plugin  string
	Feature string
	// Dir is the installed plugin's folder, and Path the role inside it, such as
	// features/hello.
	Dir  string
	Path string
	// Vars are the feature's variables, CBOR, and Secrets what it was given.
	Vars    []byte
	Secrets map[string]string
}

// Role is the Ansible role the feature is linked as.
func (f Feature) Role() string { return f.Plugin + "__" + f.Feature }

// KeyMaterial is what an apply authenticates with: the key ring's active
// private key and the host's line in known_hosts.
type KeyMaterial struct {
	// PrivateKey is a PEM private key, written as a file of mode 0600.
	PrivateKey []byte
	// KnownHost is the host's known_hosts line, written as a file of mode 0600.
	KnownHost []byte
}

// LineSink receives the run's output, one line at a time and already masked. A
// job's log is one. Its error stops the run.
type LineSink interface {
	Line(ctx context.Context, line string) error
}

// Request is one apply.
type Request struct {
	// JobID names the work directory: <state dir>/jobs/<id>.
	JobID string
	// Host is the host being applied, and Profile what it should be.
	Host    Host
	Profile profile.Profile
	// Plugins are the versions of the plugins the profile's features belong to,
	// and Rookery the server's version. The applied digest covers both.
	Plugins map[string]string
	Rookery string
	// Key is the private key and the host key to reach the host with.
	Key KeyMaterial
	// Features are the profile's features, in the order they run.
	Features []Feature
	// Secrets are the base role's secrets, and the features' own are in their
	// Feature.
	Secrets map[string]string
	// Registries are the logins the profile's registries need.
	Registries []Registry
	// AuthKey is a one-use tailnet key for this apply, empty when the profile's
	// tailnet is off.
	AuthKey string
	// LoginServer is the deployment's Headscale URL, if set.
	LoginServer string
	// Binaries are the rookeryd and box to install on the host.
	Binaries Binaries
	// Actions are the actions rookeryd offers on the host afterwards.
	Actions []Action
	// Sink receives the run's output.
	Sink LineSink
}

// Result is what an apply did.
type Result struct {
	// Digest is the applied digest of the profile it ran, for the host's record.
	Digest string
	// WorkDir is where it ran, for a caller that wants to keep the logs. It is
	// removed before Apply returns.
	WorkDir string
}

// Runner applies hosts. It holds what every apply shares: where work directories
// go, which ansible-playbook to run, and the Ansible content the roles come from.
type Runner struct {
	// StateDir is ROOKERY_STATE_DIR: work directories go under jobs/ in it.
	StateDir string
	// Playbook is the ansible-playbook executable, "ansible-playbook" when empty.
	Playbook string
	// Rookery is the server's own executable, which a tailnet host is reached
	// through. Empty asks the running executable.
	Rookery string
	// Socket is the server socket the SSH ProxyCommand connects to.
	Socket string
	// Content holds the roles and the playbook template; the embedded ansible/
	// when nil.
	Content fs.FS
}

var (
	// jobIDPattern bounds a job id to what makes a safe directory name.
	jobIDPattern = regexp.MustCompile(`^[A-Za-z0-9][A-Za-z0-9_-]*$`)
	// roleNamePattern bounds a role name to what Ansible accepts.
	hostnamePattern = regexp.MustCompile(`^[a-zA-Z0-9][a-zA-Z0-9._-]{0,63}$`)
	roleNamePattern = regexp.MustCompile(`^[a-z][a-z0-9]*(?:[-_][a-z0-9]+)*$`)
)

// Apply sets the host up as its profile says, and returns the applied digest.
func (r *Runner) Apply(ctx context.Context, req Request) (Result, error) {
	if err := r.check(req); err != nil {
		return Result{}, err
	}

	digest, err := profile.Digest(req.Profile, req.Plugins, req.Rookery)
	if err != nil {
		return Result{}, err
	}

	workDir, err := r.makeWorkDir(req.JobID)
	if err != nil {
		return Result{}, err
	}
	defer func() {
		if err := os.RemoveAll(workDir); err != nil {
			// The run's result stands; a leftover directory is the caller's to
			// see in the log, not a reason to fail an apply that worked.
			_, _ = fmt.Fprintf(os.Stderr, "apply: removing %s: %v\n", workDir, err)
		}
	}()

	if err := r.writeFiles(ctx, workDir, req); err != nil {
		return Result{}, err
	}
	if err := r.runPlaybook(ctx, workDir, req.Sink, req.masker()); err != nil {
		return Result{}, err
	}
	return Result{Digest: digest, WorkDir: workDir}, nil
}

// masker hides every secret the apply was given: the base role's, the features'
// and the registries' passwords, and the tailnet key.
func (req Request) masker() *Masker {
	values := make([]string, 0, len(req.Secrets)+len(req.Features)+len(req.Registries)+1)
	values = append(values, req.AuthKey)
	for _, secret := range req.Secrets {
		values = append(values, secret)
	}
	for _, feature := range req.Features {
		for _, secret := range feature.Secrets {
			values = append(values, secret)
		}
	}
	for _, registry := range req.Registries {
		values = append(values, registry.Password)
	}
	return NewMasker(values...)
}

// check refuses a request an apply cannot run, before anything is written.
func (r *Runner) check(req Request) error {
	if r.StateDir == "" {
		return fmt.Errorf("%w: no state directory", ErrInvalid)
	}
	if !jobIDPattern.MatchString(req.JobID) {
		return fmt.Errorf("%w: job id %q is not a name", ErrInvalid, req.JobID)
	}
	if req.Host.Name == "" {
		return fmt.Errorf("%w: the host has no name", ErrInvalid)
	}
	if !hostnamePattern.MatchString(req.Host.Name) {
		return fmt.Errorf("%w: host name %q is not a host name", ErrInvalid, req.Host.Name)
	}
	if err := req.Profile.Validate(); err != nil {
		return fmt.Errorf("%w: %w", ErrInvalid, err)
	}
	if len(req.Key.PrivateKey) == 0 {
		return fmt.Errorf("%w: no private key", ErrInvalid)
	}
	if len(req.Key.KnownHost) == 0 {
		return fmt.Errorf("%w: the host has no known key", ErrInvalid)
	}
	if req.Sink == nil {
		return fmt.Errorf("%w: no sink for the run's output", ErrInvalid)
	}
	for _, name := range []string{req.Binaries.Rookeryd, req.Binaries.Box} {
		if name == "" {
			return fmt.Errorf("%w: no rookeryd or box binary to install", ErrInvalid)
		}
		if _, err := os.Stat(name); err != nil {
			return fmt.Errorf("%w: the binary to install: %w", ErrInvalid, err)
		}
	}
	seen := make(map[string]bool, len(req.Features))
	for _, feature := range req.Features {
		if !roleNamePattern.MatchString(feature.Plugin) {
			return fmt.Errorf("%w: plugin id %q is not an id", ErrInvalid, feature.Plugin)
		}
		if !roleNamePattern.MatchString(feature.Feature) {
			return fmt.Errorf("%w: feature id %q is not an id", ErrInvalid, feature.Feature)
		}
		if seen[feature.Role()] {
			return fmt.Errorf("%w: feature %s is applied twice", ErrInvalid, feature.Role())
		}
		seen[feature.Role()] = true
		if feature.Dir == "" {
			return fmt.Errorf("%w: feature %s has no plugin folder", ErrInvalid, feature.Role())
		}
		if _, err := os.Stat(filepath.Join(feature.Dir, feature.Path)); err != nil {
			return fmt.Errorf("%w: feature %s: %w", ErrInvalid, feature.Role(), err)
		}
	}
	return nil
}

// content is the Ansible the roles come from.
func (r *Runner) content() fs.FS {
	if r.Content != nil {
		return r.Content
	}
	return ansible.FS
}

// playbook is the executable to run.
func (r *Runner) playbook() string {
	if r.Playbook != "" {
		return r.Playbook
	}
	return "ansible-playbook"
}

// rookery is the executable a tailnet host is reached through.
func (r *Runner) rookery() (string, error) {
	if r.Rookery != "" {
		return r.Rookery, nil
	}
	executable, err := os.Executable()
	if err != nil {
		return "", fmt.Errorf("apply: finding the rookery executable: %w", err)
	}
	return executable, nil
}

// makeWorkDir makes <state dir>/jobs/<id>, mode 0700, and nothing above it that
// is readable to another user.
func (r *Runner) makeWorkDir(jobID string) (string, error) {
	dir, err := filepath.Abs(filepath.Join(r.StateDir, "jobs", jobID))
	if err != nil {
		return "", fmt.Errorf("apply: resolving the work directory: %w", err)
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", fmt.Errorf("apply: making the work directory: %w", err)
	}
	// MkdirAll keeps an existing directory's mode, so it is set again.
	//nolint:gosec // the work directory holds secrets, and 0700 is its mode.
	if err := os.Chmod(dir, 0o700); err != nil {
		return "", fmt.Errorf("apply: setting the work directory's mode: %w", err)
	}
	return dir, nil
}

// roles are the roles a run applies, in order: the base, the desktop for a
// workstation, then the profile's features.
func roles(p profile.Profile, features []Feature) []string {
	names := []string{"base"}
	if p.Type == profile.TypeWorkstation {
		names = append(names, "desktop")
	}
	for _, feature := range features {
		names = append(names, feature.Role())
	}
	return names
}

// quote is a JSON string, which YAML 1.2's double-quoted scalars also are, so the
// inventory and the playbook can be written without a YAML library.
func quote(s string) string {
	var b strings.Builder
	b.WriteByte('"')
	for _, r := range s {
		switch r {
		case '"':
			b.WriteString(`\"`)
		case '\\':
			b.WriteString(`\\`)
		case '\n':
			b.WriteString(`\n`)
		case '\t':
			b.WriteString(`\t`)
		case '\r':
			b.WriteString(`\r`)
		default:
			if r < 0x20 {
				_, _ = fmt.Fprintf(&b, `\u%04x`, r)
				continue
			}
			b.WriteRune(r)
		}
	}
	b.WriteByte('"')
	return b.String()
}
