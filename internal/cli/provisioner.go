package cli

import (
	"fmt"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

func provisionerCommand() *cobra.Command {
	root := &cobra.Command{Use: "provisioner", Short: "Manage machine provisioners"}
	root.AddCommand(provisionerListCommand(true), provisionerListCommand(false))
	for _, operation := range []string{"show", "remove", "status", "resources", "add", "set", "run"} {
		root.AddCommand(provisionerOperationCommand(operation))
	}
	return root
}

func provisionerListCommand(types bool) *cobra.Command {
	name := "list"
	if types {
		name = "types"
	}
	return &cobra.Command{
		Use: name, Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			ctx, api := cmd.Context(), session.api.Provisioners
			var rows [][]string
			if types {
				out, err := api.ListTypes(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceListTypesRequest{}))
				if err != nil {
					return err
				}
				if session.printer.asJSON {
					return session.printer.message(out.Msg)
				}
				for _, item := range out.Msg.GetTypes() {
					rows = append(rows, []string{item.GetId(), item.GetLabel()})
				}
				return session.printer.table([]string{"TYPE", "LABEL"}, rows)
			}
			out, err := api.List(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceListRequest{}))
			if err != nil {
				return err
			}
			if session.printer.asJSON {
				return session.printer.message(out.Msg)
			}
			for _, item := range out.Msg.GetProvisioners() {
				rows = append(rows, provisionerRow(item))
			}
			return session.printer.table([]string{"NAME", "TYPE", "ENGINE"}, rows)
		},
	}
}

func provisionerOperationCommand(operation string) *cobra.Command {
	var node, remote string
	cmd := &cobra.Command{Use: operation + " NAME", Args: cobra.ExactArgs(1)}
	switch operation {
	case "add":
		cmd.Use, cmd.Args = "add NAME TYPE [ARG=VALUE…]", cobra.MinimumNArgs(2)
		cmd.Flags().StringVar(&node, "node", "", "managed host running the engine")
		cmd.Flags().StringVar(&remote, "remote", "", "remote engine address")
	case "set":
		cmd.Use, cmd.Args = "set NAME ARG=VALUE…", cobra.MinimumNArgs(2)
	case "run":
		cmd.Use, cmd.Args = "run NAME ACTION [ARG=VALUE…]", cobra.MinimumNArgs(2)
	}
	cmd.RunE = func(cmd *cobra.Command, args []string) error {
		s, err := open(cmd)
		if err != nil {
			return err
		}
		ctx, api, name := cmd.Context(), s.api.Provisioners, args[0]
		switch operation {
		case "show":
			out, err := api.Get(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceGetRequest{Provisioner: name}))
			if err != nil {
				return err
			}
			return s.printProvisioner(out.Msg.GetProvisioner())
		case "remove":
			_, err := api.Remove(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceRemoveRequest{Provisioner: name}))
			return err
		case "status":
			out, err := api.Status(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceStatusRequest{Provisioner: name}))
			if err != nil {
				return err
			}
			return s.printer.one(out.Msg, []string{"UP", "MESSAGE"}, []string{fmt.Sprint(out.Msg.GetUp()), out.Msg.GetMessage()})
		case "resources":
			out, err := api.ListResources(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceListResourcesRequest{Provisioner: name}))
			if err != nil {
				return err
			}
			if s.printer.asJSON {
				return s.printer.message(out.Msg)
			}
			var rows [][]string
			for _, item := range out.Msg.GetResources() {
				rows = append(rows, []string{item.GetName(), item.GetKind(), item.GetAddress()})
			}
			return s.printer.table([]string{"NAME", "KIND", "ADDRESS"}, rows)
		case "add":
			values, err := encodeFields(args[2:])
			if err != nil {
				return err
			}
			out, err := api.Add(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceAddRequest{
				Name: name, Type: args[1], Node: node, Remote: remote, ArgsCbor: values,
			}))
			if err != nil {
				return err
			}
			return s.printProvisioner(out.Msg.GetProvisioner())
		case "set":
			values, err := encodeFields(args[1:])
			if err != nil {
				return err
			}
			out, err := api.Update(ctx, connect.NewRequest(&rookeryv1.ProvisionerServiceUpdateRequest{Provisioner: name, ArgsCbor: values}))
			if err != nil {
				return err
			}
			return s.printProvisioner(out.Msg.GetProvisioner())
		case "run":
			return s.runProvisioner(cmd, name, args[1], args[2:])
		}
		return fmt.Errorf("unknown provisioner command %q", operation)
	}
	return cmd
}

func (s *session) runProvisioner(cmd *cobra.Command, name, action string, args []string) error {
	values, err := encodeFields(args)
	if err != nil {
		return err
	}
	stream, err := s.api.Provisioners.RunAction(cmd.Context(), connect.NewRequest(&rookeryv1.ProvisionerServiceRunActionRequest{
		Provisioner: name, Action: action, ArgsCbor: values,
	}))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()
	for stream.Receive() {
		if job := stream.Msg().GetJob(); job != "" {
			_ = stream.Close()
			return followJob(cmd.Context(), s.printer.out, s, job, true)
		}
		if s.printer.asJSON {
			if err := s.printer.message(stream.Msg()); err != nil {
				return err
			}
		} else if err := s.printer.line(stream.Msg().GetLine()); err != nil {
			return err
		}
	}
	return stream.Err()
}

func encodeFields(arguments []string) ([]byte, error) {
	values, err := fieldValues(arguments)
	if err != nil {
		return nil, err
	}
	return cborx.Marshal(values)
}

func (s *session) printProvisioner(entry *rookeryv1.ProvisionerEntry) error {
	return s.printer.one(entry, []string{"NAME", "TYPE", "ENGINE"}, provisionerRow(entry))
}

func provisionerRow(entry *rookeryv1.ProvisionerEntry) []string {
	return []string{entry.GetName(), entry.GetType(), firstNonEmpty(entry.GetNode(), entry.GetRemote(), "local")}
}
