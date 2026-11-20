package apply

import (
	"encoding/json"
	"fmt"
	"os"
	"regexp"
	"slices"
	"strings"

	"golang.org/x/crypto/ssh"

	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/profile"
)

// varName bounds a profile variable's name: the base role's variables, which is
// what a profile may set.
var varName = regexp.MustCompile(`^base_[a-z0-9_]+$`)

// reserved are the variables an apply sets itself, so that a profile cannot
// claim them: the key ring's key, the tailnet key, the programs, the registries,
// the secrets and the actions are the apply's to know.
var reserved = []string{
	"base_actions",
	"base_box_path",
	"base_operator",
	"base_ssh_port",
	"base_standard_user",
	"base_tailnet",
	"base_operator_key",
	"base_registries",
	"base_rookeryd_path",
	"base_secrets",
	"base_tailnet_auth_key",
}

// vars is what vars.json holds, and what the playbook reads with -e @vars.json:
// the base role's variables over its defaults, the features' under their role
// names, and what the apply itself knows.
func (r *Runner) vars(req Request) (map[string]any, error) {
	vars := make(map[string]any, len(req.Profile.Features)+8)

	fromProfile, err := decodeVars(req.Profile.Vars, "the profile")
	if err != nil {
		return nil, err
	}
	for name, value := range fromProfile {
		if !varName.MatchString(name) {
			return nil, fmt.Errorf("%w: variable %q is not a base role variable", ErrInvalid, name)
		}
		if slices.Contains(reserved, name) {
			return nil, fmt.Errorf("%w: variable %q is the apply's own", ErrInvalid, name)
		}
		vars[name] = value
	}

	// The base role refuses to run without a key, so it is derived from the one
	// the apply reaches the host with.
	key, err := publicKeyLine(req.Key.PrivateKey)
	if err != nil {
		return nil, err
	}
	operator := req.Profile.Operator
	if operator == "" {
		operator = profile.DefaultOperator
	}
	port := req.Profile.Port
	if port == 0 {
		port = profile.DefaultPort
	}
	user := profile.DefaultDesktopUser
	if desktop := req.Profile.Desktop; desktop != nil {
		user = desktop.User
		if user == "" {
			user = profile.DefaultDesktopUser
		}
		vars["desktop_user"] = user
		width, height := desktop.Width, desktop.Height
		if width == 0 {
			width = profile.DefaultWidth
		}
		if height == 0 {
			height = profile.DefaultHeight
		}
		vars["desktop_geometry"] = fmt.Sprintf("%dx%d", width, height)
		vars["desktop_gpu"] = desktop.GPU
	}
	vars["base_standard_user"] = user
	vars["base_operator"] = operator
	vars["base_ssh_port"] = port
	vars["base_operator_key"] = key
	vars["base_tailnet"] = req.Profile.Tailnet
	vars["base_tailnet_auth_key"] = req.AuthKey
	if req.LoginServer != "" {
		vars["base_tailnet_login_server"] = req.LoginServer
	}
	vars["base_rookeryd_path"] = req.Binaries.Rookeryd
	vars["base_box_path"] = req.Binaries.Box
	vars["base_actions"] = req.Actions
	vars["base_registries"] = req.Registries
	vars["base_secrets"] = req.Secrets

	for _, feature := range req.Features {
		featureVars, err := decodeVars(feature.Vars, "feature "+feature.Role())
		if err != nil {
			return nil, err
		}
		vars[feature.Role()] = map[string]any{
			"vars":    featureVars,
			"secrets": feature.Secrets,
		}
	}
	return vars, nil
}

// decodeVars decodes a CBOR map of variables, which is empty for none.
func decodeVars(data []byte, what string) (map[string]any, error) {
	if len(data) == 0 {
		return map[string]any{}, nil
	}
	var vars map[string]any
	if err := cborx.Unmarshal(data, &vars); err != nil {
		return nil, fmt.Errorf("%w: %s: %w", ErrInvalid, what, err)
	}
	literal := make(map[string]any, len(vars))
	for name, value := range vars {
		if hasTemplate(name) {
			return nil, fmt.Errorf("%w: %s contains a template in a variable name", ErrInvalid, what)
		}
		converted, err := literalVariable(value)
		if err != nil {
			return nil, fmt.Errorf("%w: %s variable %s: %w", ErrInvalid, what, name, err)
		}
		literal[name] = converted
	}
	return literal, nil
}

// literalVariable keeps profile inputs as JSON data. Ansible may interpret
// templates in extra vars on the controller, where a host manager has no shell.
func literalVariable(value any) (any, error) {
	switch value := value.(type) {
	case string:
		if hasTemplate(value) {
			return nil, fmt.Errorf("profile variables cannot contain Ansible templates")
		}
	case []any:
		for i, item := range value {
			literal, err := literalVariable(item)
			if err != nil {
				return nil, err
			}
			value[i] = literal
		}
	case map[any]any:
		object := make(map[string]any, len(value))
		for key, item := range value {
			name, ok := key.(string)
			if !ok || hasTemplate(name) {
				return nil, fmt.Errorf("object keys must be literal strings")
			}
			literal, err := literalVariable(item)
			if err != nil {
				return nil, err
			}
			object[name] = literal
		}
		return object, nil
	}
	return value, nil
}

func hasTemplate(value string) bool {
	return strings.Contains(value, "{{") || strings.Contains(value, "{%") || strings.Contains(value, "{#")
}

// publicKeyLine is the operator's authorized_keys line: the key ring's public
// key, derived from the private key the apply uses, with a comment saying where
// it came from.
func publicKeyLine(privateKey []byte) (string, error) {
	parsed, err := ssh.ParseRawPrivateKey(privateKey)
	if err != nil {
		return "", fmt.Errorf("%w: the key ring's key: %w", ErrInvalid, err)
	}
	signer, err := ssh.NewSignerFromKey(parsed)
	if err != nil {
		return "", fmt.Errorf("%w: the key ring's key: %w", ErrInvalid, err)
	}
	return strings.TrimSpace(string(ssh.MarshalAuthorizedKey(signer.PublicKey()))) + " rookery", nil
}

// writeJSON writes v as JSON, of the given mode, for a file another program
// reads.
func writeJSON(path string, v any, mode os.FileMode) error {
	data, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return fmt.Errorf("apply: encoding %s: %w", path, err)
	}
	return writeFile(path, append(data, '\n'), mode)
}
