package cli

import (
	"crypto/ed25519"
	"crypto/x509"
	"encoding/base64"
	"encoding/hex"
	"encoding/pem"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/plugins"
)

func registryCommand() *cobra.Command {
	command := &cobra.Command{Use: "registry", Short: "Manage trusted plugin registries"}
	add := &cobra.Command{
		Use: "add NAME URL", Short: "Add a registry with its Ed25519 public key", Args: cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			keyText, _ := cmd.Flags().GetString("key")
			key := decodeRegistryKey(keyText, ed25519.PublicKeySize)
			if key == nil {
				return fmt.Errorf("registry: --key needs a hex or base64 Ed25519 public key")
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			_, err = session.api.Registries.Add(cmd.Context(), connect.NewRequest(&rookeryv1.RegistryServiceAddRequest{
				Registry: &rookeryv1.Registry{Name: args[0], Url: args[1], PublicKey: key},
			}))
			return err
		},
	}
	add.Flags().String("key", "", "Ed25519 public key in hex or base64")

	list := &cobra.Command{
		Use: "list", Short: "List trusted registries", Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			found, err := session.api.Registries.List(cmd.Context(), connect.NewRequest(&rookeryv1.RegistryServiceListRequest{}))
			if err != nil {
				return err
			}
			if session.printer.asJSON {
				return session.printer.message(found.Msg)
			}
			rows := make([][]string, 0, len(found.Msg.GetRegistries()))
			for _, registry := range found.Msg.GetRegistries() {
				rows = append(rows, []string{registry.GetName(), registry.GetUrl(), hex.EncodeToString(registry.GetPublicKey())})
			}
			return session.printer.table([]string{"NAME", "URL", "KEY"}, rows)
		},
	}
	remove := &cobra.Command{
		Use: "remove NAME", Short: "Remove a registry", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			_, err = session.api.Registries.Remove(cmd.Context(), connect.NewRequest(&rookeryv1.RegistryServiceRemoveRequest{Name: args[0]}))
			return err
		},
	}
	index := &cobra.Command{
		Use: "index DIR", Short: "Write and sign a directory's package index", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			keyFile, _ := cmd.Flags().GetString("key")
			key, err := readRegistrySigningKey(keyFile)
			if err != nil {
				return err
			}
			data, signature, err := plugins.IndexDir(cmd.Context(), args[0], key)
			if err != nil {
				return err
			}
			indexPath := filepath.Join(args[0], "index.json")
			if err := os.WriteFile(indexPath, data, 0o644); err != nil { //nolint:gosec // a public registry index
				return fmt.Errorf("registry: writing index: %w", err)
			}
			if err := os.WriteFile(indexPath+".sig", signature, 0o644); err != nil { //nolint:gosec // a public signature
				return fmt.Errorf("registry: writing signature: %w", err)
			}
			_, err = fmt.Fprintln(cmd.OutOrStdout(), indexPath)
			return err
		},
	}
	index.Flags().String("key", "", "file containing the Ed25519 private key (PKCS#8 PEM or a raw seed)")
	command.AddCommand(add, list, remove, index)
	return command
}

func decodeRegistryKey(value string, size int) []byte {
	for _, decode := range []func(string) ([]byte, error){hex.DecodeString, base64.StdEncoding.DecodeString, base64.RawStdEncoding.DecodeString} {
		if key, err := decode(strings.TrimSpace(value)); err == nil && len(key) == size {
			return key
		}
	}
	return nil
}

func readRegistrySigningKey(path string) (ed25519.PrivateKey, error) {
	if path == "" {
		return nil, fmt.Errorf("registry: --key FILE is required")
	}
	data, err := os.ReadFile(path) //nolint:gosec // the caller's private key file
	if err != nil {
		return nil, fmt.Errorf("registry: reading signing key: %w", err)
	}
	if block, _ := pem.Decode(data); block != nil {
		if key, err := x509.ParsePKCS8PrivateKey(block.Bytes); err == nil {
			if signingKey, ok := key.(ed25519.PrivateKey); ok {
				return signingKey, nil
			}
		}
	}
	if seed := decodeRegistryKey(string(data), ed25519.SeedSize); seed != nil {
		return ed25519.NewKeyFromSeed(seed), nil
	}
	if len(data) == ed25519.SeedSize {
		return ed25519.NewKeyFromSeed(data), nil
	}
	return nil, fmt.Errorf("registry: the key file must contain an Ed25519 PKCS#8 private key or seed")
}
