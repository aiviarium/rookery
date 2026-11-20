package cli

import (
	"fmt"
	"io"
	"os"
	"path/filepath"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/plugins"
)

// pluginCommand is `rookery plugin`: the packages installed in a deployment,
// and the two commands that do not need a server.
func pluginCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "plugin",
		Short: "List plugins, install one, update it, remove it, enable it, disable it",
	}

	list := &cobra.Command{
		Use:   "list",
		Short: "List the installed plugins",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			listed, err := session.api.Plugins.List(cmd.Context(), connect.NewRequest(&rookeryv1.PluginServiceListRequest{}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetPlugins()))
			for _, plugin := range listed.Msg.GetPlugins() {
				enabled := "no"
				if plugin.GetEnabled() {
					enabled = "yes"
				}
				rows = append(rows, []string{plugin.GetId(), plugin.GetVersion(), plugin.GetName(), enabled})
			}
			return session.printer.table([]string{"ID", "VERSION", "NAME", "ENABLED"}, rows)
		},
	}

	add := &cobra.Command{
		Use:   "add PACKAGE|ID[@VERSION]",
		Short: "Install a package from a file, or a plugin from the registries",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			source := args[0]
			if _, err := os.Stat(source); err != nil {
				// A registry reference is resolved by the server, which knows
				// the registries and their keys; the edge API's install is a
				// package, so that arrives with the registry client.
				return fmt.Errorf("cli: %s is not a package: install from a package file until the registry client lands", source)
			}
			file, err := os.Open(source) //nolint:gosec // the file the caller named
			if err != nil {
				return fmt.Errorf("cli: reading %s: %w", source, err)
			}
			defer func() { _ = file.Close() }()

			stream := session.api.Plugins.Install(cmd.Context())
			buffer := make([]byte, 64*1024)
			for {
				read, err := file.Read(buffer)
				if read > 0 {
					if err := stream.Send(&rookeryv1.PluginServiceInstallRequest{Chunk: buffer[:read]}); err != nil {
						return err
					}
				}
				if err == io.EOF {
					break
				}
				if err != nil {
					return fmt.Errorf("cli: reading %s: %w", source, err)
				}
			}
			installed, err := stream.CloseAndReceive()
			if err != nil {
				return err
			}
			return session.printer.one(installed.Msg.GetPlugin(), []string{"ID", "VERSION", "NAME"}, []string{
				installed.Msg.GetPlugin().GetId(), installed.Msg.GetPlugin().GetVersion(), installed.Msg.GetPlugin().GetName(),
			})
		},
	}

	update := &cobra.Command{
		Use:   "update PACKAGE",
		Short: "Install a package over the plugin it replaces",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			// An install of the same id replaces it, which is what an update is;
			// resolving an id from a registry arrives with the registry client.
			return add.RunE(cmd, args)
		},
	}

	remove := &cobra.Command{
		Use:   "remove ID",
		Short: "Remove a plugin, keeping its data unless --purge",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			purge, _ := cmd.Flags().GetBool("purge")
			if _, err := session.api.Plugins.Remove(cmd.Context(), connect.NewRequest(&rookeryv1.PluginServiceRemoveRequest{Id: args[0], Purge: purge})); err != nil {
				return err
			}
			return session.printer.line("removed " + args[0])
		},
	}
	remove.Flags().Bool("purge", false, "drop the plugin's data as well")

	enable := &cobra.Command{
		Use:   "enable ID",
		Short: "Enable a plugin",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return setPluginEnabled(cmd, args[0], true)
		},
	}

	disable := &cobra.Command{
		Use:   "disable ID",
		Short: "Disable a plugin",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return setPluginEnabled(cmd, args[0], false)
		},
	}

	check := &cobra.Command{
		Use:   "check DIR",
		Short: "Check a plugin folder's manifest and files",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			manifest, err := plugins.Check(os.DirFS(args[0]))
			if err != nil {
				return err
			}
			_, err = fmt.Fprintf(cmd.OutOrStdout(), "%s %s: ok\n", manifest.ID, manifest.Version)
			return err
		},
	}

	pack := &cobra.Command{
		Use:   "pack DIR",
		Short: "Write a plugin folder as a package beside it",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			manifest, err := plugins.Check(os.DirFS(args[0]))
			if err != nil {
				return err
			}
			output, _ := cmd.Flags().GetString("output")
			if output == "" {
				output = filepath.Join(filepath.Dir(filepath.Clean(args[0])), manifest.ID+"-"+manifest.Version+".tar.gz")
			}
			file, err := os.Create(output) //nolint:gosec // the path the caller named or the package's own name
			if err != nil {
				return fmt.Errorf("cli: writing %s: %w", output, err)
			}
			packed, err := plugins.Pack(cmd.Context(), file, args[0])
			if closeErr := file.Close(); err == nil {
				err = closeErr
			}
			if err != nil {
				return err
			}
			_, err = fmt.Fprintf(cmd.OutOrStdout(), "%s %s %s\n", packed.Manifest.ID, packed.Manifest.Version, output)
			return err
		},
	}
	pack.Flags().String("output", "", "where to write the package, by default beside the folder")

	command.AddCommand(list, add, update, remove, enable, disable, check, pack)
	return command
}

// setPluginEnabled is enable and disable.
func setPluginEnabled(cmd *cobra.Command, id string, enabled bool) error {
	session, err := open(cmd)
	if err != nil {
		return err
	}
	changed, err := session.api.Plugins.SetEnabled(cmd.Context(), connect.NewRequest(&rookeryv1.PluginServiceSetEnabledRequest{Id: id, Enabled: enabled}))
	if err != nil {
		return err
	}
	return session.printer.line(changed.Msg.GetPlugin().GetId())
}
