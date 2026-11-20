package apply

import (
	"bytes"
	"fmt"
	"path/filepath"
	"text/template"

	"github.com/aiviarium/rookery/ansible"
	"github.com/aiviarium/rookery/internal/profile"
)

// ansibleConfig is the ansible.cfg every run gets: it keeps Ansible's own output
// out of the work directory, uses pipelining, and asks no questions.
const ansibleConfig = `# Written by Rookery for one apply.
[defaults]
inventory = inventory.yml
roles_path = roles
retry_files_enabled = False
nocows = True
interpreter_python = auto_silent
display_skipped_hosts = False
[ssh_connection]
pipelining = True
`

// renderInventory writes the inventory: the host, the address Ansible reaches it
// at, and the key and known host key to do it with. A host on the tailnet is
// reached at its tailnet address, through the server, which is what its
// ProxyCommand calls; a host that is not is reached at its profile's address.
func renderInventory(workDir, rookery string, req Request) ([]byte, error) {
	address := req.Profile.Address
	options := []string{
		"-o UserKnownHostsFile=" + filepath.Join(workDir, "known_hosts"),
		"-o StrictHostKeyChecking=yes",
		"-o IdentitiesOnly=yes",
	}
	if req.Host.TailnetAddress != "" {
		address = req.Host.TailnetAddress
		// ssh runs the ProxyCommand through a shell, so the path is quoted for
		// one: a space in it must not become two arguments.
		command, err := shellQuote(rookery)
		if err != nil {
			return nil, err
		}
		options = append(options, "-o ProxyCommand="+command+" server dial %h %p")
	}

	port := req.Profile.Port
	if port == 0 {
		port = profile.DefaultPort
	}
	user := req.Profile.SSHUser
	if user == "" {
		user = profile.DefaultSSHUser
	}

	common := ""
	for i, option := range options {
		if i > 0 {
			common += " "
		}
		common += option
	}

	return []byte(fmt.Sprintf(`# Written by Rookery for one apply.
all:
  hosts:
    %s:
      ansible_connection: ssh
      ansible_host: %s
      ansible_port: %d
      ansible_user: %s
      ansible_ssh_private_key_file: %s
      ansible_ssh_common_args: %s
`,
		quote(req.Host.Name),
		quote(address),
		port,
		quote(user),
		quote(filepath.Join(workDir, "key")),
		quote(common),
	)), nil
}

// renderPlaybook writes the playbook: the roles to apply, in order.
func (r *Runner) renderPlaybook(req Request) ([]byte, error) {
	parsed, err := template.ParseFS(r.content(), ansible.PlaybookTemplate)
	if err != nil {
		return nil, fmt.Errorf("apply: reading the playbook template: %w", err)
	}
	var buffer bytes.Buffer
	if err := parsed.Execute(&buffer, struct {
		HostName string
		Roles    []string
	}{
		HostName: req.Host.Name,
		Roles:    roles(req.Profile, req.Features),
	}); err != nil {
		return nil, fmt.Errorf("apply: rendering the playbook: %w", err)
	}
	return buffer.Bytes(), nil
}

// shellQuote quotes a path for the shell that runs an SSH ProxyCommand.
func shellQuote(path string) (string, error) {
	if path == "" {
		return "", fmt.Errorf("%w: no rookery executable for the ProxyCommand", ErrInvalid)
	}
	for _, r := range path {
		if !pathRune(r) {
			return "", fmt.Errorf("%w: the rookery executable's path %q cannot be quoted for ssh", ErrInvalid, path)
		}
	}
	return path, nil
}

// pathRune reports whether a rune is one a ProxyCommand's path may hold without
// quoting: the characters of a plain Unix path.
func pathRune(r rune) bool {
	switch {
	case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9':
		return true
	case r == '/', r == '.', r == '_', r == '-', r == '+':
		return true
	default:
		return false
	}
}
