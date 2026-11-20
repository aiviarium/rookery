package sdk

import (
	"encoding/json"
	"fmt"
	"os"
)

// Secrets reads the secrets the server gave this process: the values of the
// names the provisioner declared in its description.
//
// They are in the file ROOKERY_SECRETS names, a JSON object of name to value of
// mode 0600, rather than in the environment or on a command line. It returns an
// empty map when the server gave none.
func Secrets() (map[string]string, error) {
	config, err := ConfigFromEnv()
	if err != nil {
		return nil, err
	}
	return ReadSecrets(config.SecretsFile)
}

// ReadSecrets reads a secrets file.
func ReadSecrets(path string) (map[string]string, error) {
	if path == "" {
		return map[string]string{}, nil
	}
	//nolint:gosec // the path is the server's, in a directory only this user may read.
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("sdk: reading the secrets: %w", err)
	}
	secrets := map[string]string{}
	if err := json.Unmarshal(data, &secrets); err != nil {
		return nil, fmt.Errorf("sdk: the secrets file is not a JSON object of name to value: %w", err)
	}
	return secrets, nil
}
