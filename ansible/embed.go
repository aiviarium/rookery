// Package ansible carries the Ansible content an apply extracts: the base role
// every managed host gets, the desktop role workstations add, and the template
// of the playbook that puts them on a host.
//
// The roles are written for Ubuntu 24.04 and Debian 12, and use apt, systemd and
// ansible.builtin only: the server's image installs ansible-core, not the
// community collections. site.yml.j2 is rendered by internal/apply with Go's
// text/template, not by Jinja, whatever its suffix says.
package ansible

import "embed"

// FS holds the roles and the playbook template. internal/apply extracts the
// roles into its work directory and renders the template into site.yml.
//
//go:embed roles site.yml.j2
var FS embed.FS

// PlaybookTemplate is the path of the playbook template in FS.
const PlaybookTemplate = "site.yml.j2"

// RolesDir is the directory of the roles in FS.
const RolesDir = "roles"
