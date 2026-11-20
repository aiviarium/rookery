package agent

import (
	"fmt"
	"os"
	"strings"
)

// readEnvFile reads a KEY=VALUE file, as the desktop's environment file is
// written: one assignment per line, '#' comments, and values optionally quoted.
// It returns an error when the file is not there, which a caller reports as the
// desktop not being set up.
func readEnvFile(path string) (map[string]string, error) {
	raw, err := os.ReadFile(path) //nolint:gosec // the daemon's own configuration file.
	if err != nil {
		return nil, fmt.Errorf("agent: reading %s: %w", path, err)
	}

	env := make(map[string]string)
	for _, line := range strings.Split(string(raw), "\n") {
		line = strings.TrimSpace(line)
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		name, value, found := strings.Cut(line, "=")
		if !found {
			continue
		}
		name = strings.TrimSpace(name)
		value = strings.TrimSpace(value)
		if len(value) >= 2 {
			if (value[0] == '"' && value[len(value)-1] == '"') || (value[0] == '\'' && value[len(value)-1] == '\'') {
				value = value[1 : len(value)-1]
			}
		}
		if name != "" {
			env[name] = value
		}
	}
	return env, nil
}
