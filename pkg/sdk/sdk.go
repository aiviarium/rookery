// Package sdk is the Go SDK for Rookery's plugins: what a plugin process needs
// to talk to the server, and to be talked to.
//
// The server starts a plugin's executable and names two sockets in its
// environment. On ROOKERY_PLUGIN_SOCKET the plugin serves what the server calls:
// today a provisioner (ServeProvisioner), and with the plugin system its service
// and procedures. On ROOKERY_SOCKET it calls the server, with
// ROOKERY_PLUGIN_TOKEN as its credential: NewClient gives that, and Node gives a
// dialer that reaches an engine's socket on a managed host through the server.
//
// Dynamic values on the wire are CBOR in Core Deterministic mode; Marshal and
// Unmarshal are the helpers for the bytes fields that carry them.
//
// StartHarness and Harness are the test side: a stand-in for the server's socket,
// and a provisioner process started against it.
package sdk

import (
	"errors"
	"fmt"
	"os"
)

// The variables the server gives a plugin process.
const (
	// SocketEnv names the server's Unix socket, which the plugin calls.
	SocketEnv = "ROOKERY_SOCKET"

	// PluginSocketEnv names the Unix socket the plugin serves on.
	PluginSocketEnv = "ROOKERY_PLUGIN_SOCKET"

	// TokenEnv names the credential the plugin presents on the server's socket.
	TokenEnv = "ROOKERY_PLUGIN_TOKEN" //nolint:gosec // the variable's name, not a credential.

	// SecretsEnv names the file a provisioner's secrets are in when the server
	// gives it any: a JSON object of name to value, mode 0600. A value is never
	// in the environment or on a command line.
	SecretsEnv = "ROOKERY_SECRETS"
)

// ErrConfig is the sentinel every refusal of a plugin's environment wraps.
var ErrConfig = errors.New("sdk: configuration")

// Config is what the server gives a plugin process.
type Config struct {
	// Socket is the server's Unix socket, ROOKERY_SOCKET.
	Socket string

	// PluginSocket is the socket this process serves on, ROOKERY_PLUGIN_SOCKET.
	PluginSocket string

	// Token is the credential to present on the server's socket,
	// ROOKERY_PLUGIN_TOKEN.
	Token string

	// SecretsFile is where the values of the secrets the provisioner declared
	// are, ROOKERY_SECRETS. Empty when it declared none.
	SecretsFile string
}

// ConfigFromEnv reads the configuration the server wrote into the environment.
func ConfigFromEnv() (Config, error) {
	config := Config{
		Socket:       os.Getenv(SocketEnv),
		PluginSocket: os.Getenv(PluginSocketEnv),
		Token:        os.Getenv(TokenEnv),
		SecretsFile:  os.Getenv(SecretsEnv),
	}
	if config.Socket == "" {
		return Config{}, fmt.Errorf("%w: %s is not set: a plugin is started by the server", ErrConfig, SocketEnv)
	}
	if config.PluginSocket == "" {
		return Config{}, fmt.Errorf("%w: %s is not set: a plugin is started by the server", ErrConfig, PluginSocketEnv)
	}
	return config, nil
}
