package apply

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// writeFiles writes everything an apply runs with into the work directory: the
// key, the known host key, the roles, the inventory, the playbook, ansible.cfg
// and the variables.
func (r *Runner) writeFiles(ctx context.Context, workDir string, req Request) error {
	// The key is what the inventory authenticates with, so it is the first thing
	// written and the last thing readable by anyone else.
	if err := writeFile(filepath.Join(workDir, "key"), req.Key.PrivateKey, 0o600); err != nil {
		return err
	}
	if err := writeFile(filepath.Join(workDir, "known_hosts"), req.Key.KnownHost, 0o600); err != nil {
		return err
	}
	if err := os.MkdirAll(filepath.Join(workDir, "home"), 0o700); err != nil {
		return fmt.Errorf("apply: making the run's home: %w", err)
	}

	if err := r.writeRoles(workDir, req.Features); err != nil {
		return err
	}

	rookery, err := r.rookery()
	if err != nil {
		return err
	}
	inventory, err := renderInventory(workDir, rookery, req)
	if err != nil {
		return err
	}
	if err := writeFile(filepath.Join(workDir, "inventory.yml"), inventory, 0o600); err != nil {
		return err
	}

	playbook, err := r.renderPlaybook(req)
	if err != nil {
		return err
	}
	if err := writeFile(filepath.Join(workDir, "site.yml"), playbook, 0o644); err != nil {
		return err
	}
	if err := writeFile(filepath.Join(workDir, "ansible.cfg"), []byte(ansibleConfig), 0o644); err != nil {
		return err
	}

	vars, err := r.vars(req)
	if err != nil {
		return err
	}
	if err := writeJSON(filepath.Join(workDir, "vars.json"), vars, 0o600); err != nil {
		return err
	}
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("apply: writing the work directory: %w", err)
	}
	return nil
}

// writeRoles extracts the embedded roles and links each feature's tasks in as a
// role of its own, so a feature runs from the plugin folder it was installed in.
func (r *Runner) writeRoles(workDir string, features []Feature) error {
	rolesDir := filepath.Join(workDir, "roles")
	//nolint:gosec // the work directory is 0700, so nothing else can reach the roles in it.
	if err := os.MkdirAll(rolesDir, 0o755); err != nil {
		return fmt.Errorf("apply: making the roles directory: %w", err)
	}
	if _, err := fs.Stat(r.content(), "roles/base"); err != nil {
		return fmt.Errorf("%w: %w", errNoRole, err)
	}
	if err := extractRoles(r.content(), rolesDir); err != nil {
		return err
	}

	for _, feature := range features {
		target, err := filepath.Abs(filepath.Join(feature.Dir, feature.Path))
		if err != nil {
			return fmt.Errorf("apply: resolving feature %s: %w", feature.Role(), err)
		}
		link := filepath.Join(rolesDir, feature.Role())
		if _, err := os.Lstat(link); err == nil {
			return fmt.Errorf("apply: the role %s is both bundled and a feature", feature.Role())
		}
		if err := os.Symlink(target, link); err != nil {
			return fmt.Errorf("apply: linking feature %s: %w", feature.Role(), err)
		}
	}
	return nil
}

// extractRoles writes the roles in content into dir, as the server's user, with
// the modes they were written with.
func extractRoles(content fs.FS, dir string) error {
	root := "roles"
	err := fs.WalkDir(content, root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return fmt.Errorf("apply: reading %s: %w", path, err)
		}
		if path == root {
			return nil
		}
		target := filepath.Join(dir, strings.TrimPrefix(path, root+"/"))
		if entry.IsDir() {
			//nolint:gosec // as above: the work directory is 0700.
			if err := os.MkdirAll(target, 0o755); err != nil {
				return fmt.Errorf("apply: making %s: %w", target, err)
			}
			return nil
		}
		data, err := fs.ReadFile(content, path)
		if err != nil {
			return fmt.Errorf("apply: reading %s: %w", path, err)
		}
		return writeFile(target, data, 0o644)
	})
	if err != nil {
		return err
	}
	return nil
}

// writeFile writes a file of the given mode, replacing it, and makes its parent.
func writeFile(path string, data []byte, mode os.FileMode) error {
	//nolint:gosec // the work directory is 0700, so nothing else can reach these in it.
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return fmt.Errorf("apply: making the directory of %s: %w", path, err)
	}
	// The file's mode is set at creation and again afterwards, since a file that
	// was there already keeps its own.
	//nolint:gosec // the mode is the caller's decision: 0600 for secrets, 0644 for a playbook.
	if err := os.WriteFile(path, data, mode); err != nil {
		return fmt.Errorf("apply: writing %s: %w", path, err)
	}
	if err := os.Chmod(path, mode); err != nil {
		return fmt.Errorf("apply: setting the mode of %s: %w", path, err)
	}
	return nil
}

// errNoRole is what a bundle without the base role reports.
var errNoRole = errors.New("apply: the bundled Ansible has no base role")
