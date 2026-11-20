package cli

import (
	"fmt"
	"io"
	"os"
	"os/exec"
	"sort"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/profile"
)

// hostCommand is `rookery host`: the machines Rookery runs.
func hostCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "host",
		Short: "The machines Rookery runs",
	}
	command.AddCommand(
		hostListCommand(),
		hostShowCommand(),
		hostAddCommand(),
		hostCreateCommand(),
		hostRemoveCommand(),
		hostRenameCommand(),
		hostProfileCommand(),
		hostLabelCommand(),
		hostCheckCommand(),
	)
	return command
}

// hostListCommand is `rookery host list`.
func hostListCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "list",
		Short: "List hosts",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			labels, err := cmd.Flags().GetStringArray("label")
			if err != nil {
				return err
			}
			request := &rookeryv1.HostServiceListRequest{Label: map[string]string{}}
			for _, pair := range labels {
				key, value, found := strings.Cut(pair, "=")
				if !found {
					return fmt.Errorf("cli: --label wants KEY=VALUE, not %q", pair)
				}
				request.Label[key] = value
			}
			listed, err := session.api.Hosts.List(cmd.Context(), connect.NewRequest(request))
			if err != nil {
				return err
			}

			rows := make([][]string, 0, len(listed.Msg.GetHosts()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetHosts()))
			for _, host := range listed.Msg.GetHosts() {
				status := "current"
				if host.GetAppliedDigest() == "" {
					status = "never applied"
				} else if host.GetDigest() != "" && host.GetAppliedDigest() != host.GetDigest() {
					status = "behind"
				}
				rows = append(rows, []string{host.GetName(), hostTypeName(host.GetType()), host.GetProfile().GetAddress(), status, strings.Join(labelsOf(host.GetLabels()), " ")})
				messages = append(messages, host)
			}
			return session.printer.list(messages, []string{"NAME", "TYPE", "ADDRESS", "STATE", "LABELS"}, rows)
		},
	}
	command.Flags().StringArray("label", nil, "only hosts with this label, as KEY=VALUE; repeatable")
	return command
}

// hostShowCommand is `rookery host show`.
func hostShowCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "show NAME",
		Short: "Show a host",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			got, err := session.api.Hosts.Get(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceGetRequest{Name: args[0]}))
			if err != nil {
				return err
			}
			return session.printer.message(got.Msg.GetHost())
		},
	}
}

// hostAddCommand is `rookery host add`.
func hostAddCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "add NAME ADDRESS",
		Short: "Add a host",
		Long: `Add a host.

A password or a private key given with --password-stdin or --key lets first
contact install the server's key, as a job whose log is followed. Without one,
the host is recorded and an apply or a later check contacts it.`,
		Args: cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			hostType, err := cmd.Flags().GetString("type")
			if err != nil {
				return err
			}
			user, err := cmd.Flags().GetString("user")
			if err != nil {
				return err
			}
			port, err := cmd.Flags().GetInt("port")
			if err != nil {
				return err
			}
			credential, err := credentialFromFlags(cmd)
			if err != nil {
				return err
			}

			request := &rookeryv1.HostServiceAddRequest{
				Name:       args[0],
				Address:    args[1],
				User:       user,
				Port:       portInt32(port),
				Credential: credential,
			}
			if hostType != "" {
				parsed, err := parseHostType(hostType)
				if err != nil {
					return err
				}
				request.Type = parsed
			}
			added, err := session.api.Hosts.Add(cmd.Context(), connect.NewRequest(request))
			if err != nil {
				return err
			}
			if err := session.printer.message(added.Msg.GetHost()); err != nil {
				return err
			}
			if jobID := added.Msg.GetFirstContact(); jobID != "" {
				if _, err := fmt.Fprintf(cmd.OutOrStdout(), "first contact is job %s\n", jobID); err != nil {
					return err
				}
				return followJob(cmd.Context(), cmd.OutOrStdout(), session, jobID, true)
			}
			return nil
		},
	}
	command.Flags().String("type", "ssh", "the host's type: ssh, managed or workstation")
	command.Flags().String("user", "", "the account the server logs in as, ops by default")
	command.Flags().Int("port", 0, "the SSH port, 22 by default")
	command.Flags().Bool("password-stdin", false, "read the account's password from stdin for first contact")
	command.Flags().String("key", "", "a private key file for first contact")
	return command
}

// credentialFromFlags reads the one-time credential first contact logs in with.
func credentialFromFlags(cmd *cobra.Command) (*rookeryv1.Credential, error) {
	stdin, err := cmd.Flags().GetBool("password-stdin")
	if err != nil {
		return nil, err
	}
	keyPath, err := cmd.Flags().GetString("key")
	if err != nil {
		return nil, err
	}
	switch {
	case stdin && keyPath != "":
		return nil, fmt.Errorf("cli: give a password or a key, not both")
	case stdin:
		raw, err := io.ReadAll(cmd.InOrStdin())
		if err != nil {
			return nil, fmt.Errorf("cli: reading the password: %w", err)
		}
		return &rookeryv1.Credential{Kind: &rookeryv1.Credential_Password{Password: strings.TrimRight(string(raw), "\r\n")}}, nil
	case keyPath != "":
		raw, err := os.ReadFile(keyPath) //nolint:gosec // the operator's own key
		if err != nil {
			return nil, fmt.Errorf("cli: reading %s: %w", keyPath, err)
		}
		return &rookeryv1.Credential{Kind: &rookeryv1.Credential_PrivateKey{PrivateKey: raw}}, nil
	default:
		return nil, nil
	}
}

// hostCreateCommand is `rookery host create`: a host made by a provisioner.
func hostCreateCommand() *cobra.Command {
	var provisioner, hostType string
	command := &cobra.Command{
		Use:   "create NAME --provisioner P [ARG=VALUE…]",
		Short: "Create a host's machine through a provisioner",
		Long: `Create a host's machine through a provisioner.

The machine's seed has the operator account with the key ring's public key and
the tailnet key; the host is recorded when the machine is made, and an apply
sets the rest up.`,
		Args: cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			if provisioner == "" {
				return fmt.Errorf("cli: host create needs --provisioner")
			}
			values, err := encodeFields(args[1:])
			if err != nil {
				return err
			}
			request := &rookeryv1.ProvisionerServiceCreateMachineRequest{
				Host:        args[0],
				Provisioner: provisioner,
				ArgsCbor:    values,
			}
			if hostType != "" {
				parsed, err := parseHostType(hostType)
				if err != nil {
					return err
				}
				request.Type = parsed
			}
			created, err := session.api.Provisioners.CreateMachine(cmd.Context(), connect.NewRequest(request))
			if err != nil {
				return err
			}
			if job := created.Msg.GetJob(); job != "" {
				if _, err := fmt.Fprintf(cmd.OutOrStdout(), "creating %s is job %s\n", args[0], job); err != nil {
					return err
				}
				return followJob(cmd.Context(), cmd.OutOrStdout(), session, job, true)
			}
			return session.printer.message(created.Msg.GetMachine())
		},
	}
	command.Flags().StringVar(&provisioner, "provisioner", "", "the provisioner entry that makes the machine")
	command.Flags().StringVar(&hostType, "type", "", "the host's type: ssh (default), managed or workstation")
	return command
}

// hostRemoveCommand is `rookery host remove`.
func hostRemoveCommand() *cobra.Command {
	var machine bool
	command := &cobra.Command{
		Use:   "remove NAME",
		Short: "Remove a host, or a host with its machine",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			if machine {
				removed, err := session.api.Provisioners.RemoveMachine(cmd.Context(), connect.NewRequest(&rookeryv1.ProvisionerServiceRemoveMachineRequest{Host: args[0]}))
				if err != nil {
					return err
				}
				if job := removed.Msg.GetJob(); job != "" {
					if err := followJob(cmd.Context(), cmd.OutOrStdout(), session, job, true); err != nil {
						return err
					}
				}
				return session.printer.line(args[0], "removed")
			}
			if _, err := session.api.Hosts.Remove(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceRemoveRequest{Name: args[0]})); err != nil {
				return err
			}
			return session.printer.line(args[0], "removed")
		},
	}
	command.Flags().BoolVar(&machine, "machine", false, "remove the host's machine through its provisioner as well")
	return command
}

// hostRenameCommand is `rookery host rename`.
func hostRenameCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "rename NAME NEW",
		Short: "Rename a host, with its grants",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			renamed, err := session.api.Hosts.Rename(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceRenameRequest{Name: args[0], NewName: args[1]}))
			if err != nil {
				return err
			}
			return session.printer.message(renamed.Msg.GetHost())
		},
	}
}

// hostProfileCommand is `rookery host profile`: show a profile, or open an
// editor on its JSON.
func hostProfileCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "profile NAME [--edit]",
		Short: "Show a host's profile, or edit it",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			got, err := session.api.Hosts.Get(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceGetRequest{Name: args[0]}))
			if err != nil {
				return err
			}
			edit, err := cmd.Flags().GetBool("edit")
			if err != nil {
				return err
			}
			if !edit {
				return session.printer.message(got.Msg.GetHost().GetProfile())
			}
			editedMessage, err := editJSON(cmd, got.Msg.GetHost().GetProfile())
			if err != nil {
				return err
			}
			edited, ok := editedMessage.(*rookeryv1.HostProfile)
			if !ok {
				return fmt.Errorf("cli: the editor produced no profile")
			}
			updated, err := session.api.Hosts.SetProfile(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceSetProfileRequest{
				Name:    args[0],
				Profile: edited,
			}))
			if err != nil {
				return err
			}
			return session.printer.message(updated.Msg.GetHost())
		},
	}
	command.Flags().Bool("edit", false, "edit the profile as protobuf JSON in $EDITOR")
	return command
}

// hostLabelCommand is `rookery host label`: put labels, or take them away.
func hostLabelCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "label NAME K=V… [--remove K…]",
		Short: "Put labels on a host, or take them away",
		Args:  cobra.MinimumNArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			remove, err := cmd.Flags().GetStringArray("remove")
			if err != nil {
				return err
			}
			request := &rookeryv1.HostServiceSetLabelsRequest{Name: args[0], Set: map[string]string{}, Remove: remove}
			for _, pair := range args[1:] {
				key, value, found := strings.Cut(pair, "=")
				if !found {
					return fmt.Errorf("cli: a label is KEY=VALUE, not %q", pair)
				}
				request.Set[key] = value
			}
			updated, err := session.api.Hosts.SetLabels(cmd.Context(), connect.NewRequest(request))
			if err != nil {
				return err
			}
			labels := labelsOf(updated.Msg.GetHost().GetLabels())
			return session.printer.line(args[0], strings.Join(labels, " "))
		},
	}
	command.Flags().StringArray("remove", nil, "a label's key to take away; repeatable")
	return command
}

// hostCheckCommand is `rookery host check`: whether a host can do something.
func hostCheckCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "check NAME CAPABILITY",
		Short: "Whether a host can do a capability, and what would change the answer",
		Long:  "Whether a host can do a capability: contact, apply, exec, terminal, files, ports, desktop, channel or actions.",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			capability, err := capabilityFromName(args[1])
			if err != nil {
				return err
			}
			checked, err := session.api.Hosts.Check(cmd.Context(), connect.NewRequest(&rookeryv1.HostServiceCheckRequest{
				Name:       args[0],
				Capability: capability,
			}))
			if err != nil {
				return err
			}
			result := "yes"
			if !checked.Msg.GetAllowed() {
				result = "no: " + checked.Msg.GetReason()
			}
			return session.printer.line(args[0], hostTypeName(checked.Msg.GetType())+", "+args[1]+": "+result)
		},
	}
}

// keyCommand is `rookery key`: the ring and the trusted host keys.
func keyCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "key",
		Short: "The server's SSH keys and the host keys it trusts",
	}

	list := &cobra.Command{
		Use:   "list",
		Short: "List the keys of the ring",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			listed, err := session.api.Keys.List(cmd.Context(), connect.NewRequest(&rookeryv1.KeyServiceListRequest{}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetKeys()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetKeys()))
			for _, key := range listed.Msg.GetKeys() {
				state := "retired"
				if key.GetActive() {
					state = "active"
				}
				rows = append(rows, []string{key.GetId(), key.GetName(), key.GetType(), key.GetFingerprint(), state})
				messages = append(messages, key)
			}
			return session.printer.list(messages, []string{"ID", "NAME", "TYPE", "FINGERPRINT", "STATE"}, rows)
		},
	}

	rotate := &cobra.Command{
		Use:   "rotate",
		Short: "Make a new key active and retire the old one",
		Long: `Make a new key active and retire the old one.

The old key stays until no host's authorized_keys names it: an apply puts the new
one there. A host that only has the old one still logs in with it until then.`,
		Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			name, err := cmd.Flags().GetString("name")
			if err != nil {
				return err
			}
			rotated, err := session.api.Keys.Rotate(cmd.Context(), connect.NewRequest(&rookeryv1.KeyServiceRotateRequest{Name: name}))
			if err != nil {
				return err
			}
			key := rotated.Msg.GetKey()
			return session.printer.line(key.GetId(), key.GetFingerprint()+" active; apply the hosts to install it")
		},
	}
	rotate.Flags().String("name", "default", "the new key's name")

	trust := &cobra.Command{
		Use:   "trust ADDRESS",
		Short: "Trust the host key a host offered when it did not match",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			known, err := session.api.Keys.ListKnownHosts(cmd.Context(), connect.NewRequest(&rookeryv1.KeyServiceListKnownHostsRequest{}))
			if err != nil {
				return err
			}
			var pending string
			for _, host := range known.Msg.GetHosts() {
				if host.GetAddress() == args[0] {
					pending = host.GetPendingFingerprint()
				}
			}
			if pending == "" {
				return fmt.Errorf("cli: %s offered no new key", args[0])
			}
			trusted, err := session.api.Keys.TrustHostKey(cmd.Context(), connect.NewRequest(&rookeryv1.KeyServiceTrustHostKeyRequest{
				Address:     args[0],
				Fingerprint: pending,
			}))
			if err != nil {
				return err
			}
			return session.printer.line(args[0], trusted.Msg.GetHost().GetFingerprint()+" trusted")
		},
	}

	hosts := &cobra.Command{
		Use:   "hosts",
		Short: "List the host keys the server trusts",
		Args:  cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			listed, err := session.api.Keys.ListKnownHosts(cmd.Context(), connect.NewRequest(&rookeryv1.KeyServiceListKnownHostsRequest{}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetHosts()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetHosts()))
			for _, host := range listed.Msg.GetHosts() {
				rows = append(rows, []string{host.GetAddress(), host.GetHost(), host.GetFingerprint(), host.GetPendingFingerprint()})
				messages = append(messages, host)
			}
			return session.printer.list(messages, []string{"ADDRESS", "HOST", "FINGERPRINT", "OFFERED"}, rows)
		},
	}

	forget := &cobra.Command{
		Use:   "forget ADDRESS",
		Short: "Forget a trusted host key, so the next link trusts what it is offered",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			if _, err := session.api.Keys.ForgetHostKey(cmd.Context(), connect.NewRequest(&rookeryv1.KeyServiceForgetHostKeyRequest{Address: args[0]})); err != nil {
				return err
			}
			return session.printer.line(args[0], "forgotten")
		},
	}

	command.AddCommand(list, rotate, trust, hosts, forget)
	return command
}

// editJSON opens a message as protobuf JSON in the operator's editor and reads
// the edited message back.
func editJSON(cmd *cobra.Command, message proto.Message) (proto.Message, error) {
	editor := os.Getenv("EDITOR")
	if editor == "" {
		editor = "vi"
	}
	options := protojson.MarshalOptions{Multiline: true, Indent: "  "}
	raw, err := options.Marshal(message)
	if err != nil {
		return nil, fmt.Errorf("cli: writing the profile: %w", err)
	}
	file, err := os.CreateTemp("", "rookery-profile-*.json")
	if err != nil {
		return nil, fmt.Errorf("cli: making a temporary file: %w", err)
	}
	path := file.Name()
	defer func() { _ = os.Remove(path) }()
	if _, err := file.Write(append(raw, '\n')); err != nil {
		_ = file.Close()
		return nil, fmt.Errorf("cli: writing %s: %w", path, err)
	}
	if err := file.Close(); err != nil {
		return nil, fmt.Errorf("cli: writing %s: %w", path, err)
	}

	run := exec.CommandContext(cmd.Context(), editor, path) //nolint:gosec // the operator's EDITOR
	run.Stdin = cmd.InOrStdin()
	run.Stdout = cmd.OutOrStdout()
	run.Stderr = cmd.ErrOrStderr()
	if err := run.Run(); err != nil {
		return nil, fmt.Errorf("cli: %s: %w", editor, err)
	}

	edited, err := os.ReadFile(path) //nolint:gosec // the file this command wrote
	if err != nil {
		return nil, fmt.Errorf("cli: reading %s: %w", path, err)
	}
	profile := &rookeryv1.HostProfile{}
	if err := protojson.Unmarshal(edited, profile); err != nil {
		return nil, fmt.Errorf("cli: reading the edited profile: %w", err)
	}
	return profile, nil
}

// portInt32 is a port flag as the proto field: a flag that is out of range is
// refused by the server.
func portInt32(port int) int32 {
	if port < 0 || port > 65535 {
		return 0
	}
	return int32(port) //nolint:gosec // clamped just above
}

// hostTypeName is a host type in words.
func hostTypeName(t rookeryv1.HostType) string {
	switch t {
	case rookeryv1.HostType_HOST_TYPE_SSH:
		return "ssh"
	case rookeryv1.HostType_HOST_TYPE_MANAGED:
		return "managed"
	case rookeryv1.HostType_HOST_TYPE_WORKSTATION:
		return "workstation"
	default:
		return "unknown"
	}
}

// parseHostType reads a host type by name.
func parseHostType(text string) (rookeryv1.HostType, error) {
	parsed, err := profile.ParseType(text)
	if err != nil {
		return rookeryv1.HostType_HOST_TYPE_UNSPECIFIED, err
	}
	switch parsed {
	case profile.TypeSSH:
		return rookeryv1.HostType_HOST_TYPE_SSH, nil
	case profile.TypeManaged:
		return rookeryv1.HostType_HOST_TYPE_MANAGED, nil
	case profile.TypeWorkstation:
		return rookeryv1.HostType_HOST_TYPE_WORKSTATION, nil
	default:
		return rookeryv1.HostType_HOST_TYPE_UNSPECIFIED, fmt.Errorf("%w: %q", access.ErrInvalid, text)
	}
}

// capabilityFromName reads a capability by name.
func capabilityFromName(text string) (rookeryv1.Capability, error) {
	switch strings.ToLower(text) {
	case "contact":
		return rookeryv1.Capability_CAPABILITY_UNSPECIFIED, nil
	case "apply":
		return rookeryv1.Capability_CAPABILITY_APPLY, nil
	case "exec":
		return rookeryv1.Capability_CAPABILITY_EXEC, nil
	case "terminal":
		return rookeryv1.Capability_CAPABILITY_TERMINAL, nil
	case "files":
		return rookeryv1.Capability_CAPABILITY_FILES, nil
	case "ports":
		return rookeryv1.Capability_CAPABILITY_PORTS, nil
	case "desktop":
		return rookeryv1.Capability_CAPABILITY_DESKTOP, nil
	case "channel":
		return rookeryv1.Capability_CAPABILITY_CHANNEL, nil
	case "actions":
		return rookeryv1.Capability_CAPABILITY_ACTIONS, nil
	default:
		return rookeryv1.Capability_CAPABILITY_UNSPECIFIED, fmt.Errorf("cli: %q is not a capability", text)
	}
}

// labelsOf is a label map as KEY=VALUE pairs, sorted.
func labelsOf(labels map[string]string) []string {
	keys := make([]string, 0, len(labels))
	for key := range labels {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	pairs := make([]string, 0, len(keys))
	for _, key := range keys {
		pairs = append(pairs, key+"="+labels[key])
	}
	return pairs
}
