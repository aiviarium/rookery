package cli

import (
	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// spaceCommand is `rookery space`: the containers records live in.
func spaceCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "space",
		Short: "List spaces, make them, rename them, archive them",
	}

	command.AddCommand(
		&cobra.Command{
			Use:   "list",
			Short: "List spaces",
			RunE: func(cmd *cobra.Command, _ []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				includeArchived, err := cmd.Flags().GetBool("archived")
				if err != nil {
					return err
				}
				listed, err := session.api.Spaces.List(cmd.Context(), connect.NewRequest(&rookeryv1.SpaceServiceListRequest{
					IncludeArchived: includeArchived,
				}))
				if err != nil {
					return err
				}
				rows := make([][]string, 0, len(listed.Msg.GetSpaces()))
				messages := make([]proto.Message, 0, len(listed.Msg.GetSpaces()))
				for _, space := range listed.Msg.GetSpaces() {
					state := ""
					if space.GetArchived() {
						state = "archived"
					}
					rows = append(rows, []string{space.GetId(), space.GetKind(), space.GetName(), state})
					messages = append(messages, space)
				}
				return session.printer.list(messages, []string{"ID", "KIND", "NAME", "STATE"}, rows)
			},
		},
		&cobra.Command{
			Use:   "kinds",
			Short: "List the kinds of space this deployment declares",
			RunE: func(cmd *cobra.Command, _ []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				listed, err := session.api.Spaces.ListKinds(cmd.Context(), connect.NewRequest(&rookeryv1.SpaceServiceListKindsRequest{}))
				if err != nil {
					return err
				}
				rows := make([][]string, 0, len(listed.Msg.GetKinds()))
				messages := make([]proto.Message, 0, len(listed.Msg.GetKinds()))
				for _, kind := range listed.Msg.GetKinds() {
					rows = append(rows, []string{kind.GetId(), kind.GetLabel(), kind.GetPlugin()})
					messages = append(messages, kind)
				}
				return session.printer.list(messages, []string{"ID", "LABEL", "PLUGIN"}, rows)
			},
		},
		&cobra.Command{
			Use:   "create KIND NAME",
			Short: "Make a space",
			Args:  cobra.ExactArgs(2),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				made, err := session.api.Spaces.Create(cmd.Context(), connect.NewRequest(&rookeryv1.SpaceServiceCreateRequest{
					Kind: args[0],
					Name: args[1],
				}))
				if err != nil {
					return err
				}
				space := made.Msg.GetSpace()
				return session.printer.one(space, []string{"ID", "KIND", "NAME"}, []string{space.GetId(), space.GetKind(), space.GetName()})
			},
		},
		&cobra.Command{
			Use:   "show ID",
			Short: "Show one space",
			Args:  cobra.ExactArgs(1),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				found, err := session.api.Spaces.Get(cmd.Context(), connect.NewRequest(&rookeryv1.SpaceServiceGetRequest{Space: args[0]}))
				if err != nil {
					return err
				}
				space := found.Msg.GetSpace()
				return session.printer.one(space, []string{"ID", "KIND", "NAME", "STATE"},
					[]string{space.GetId(), space.GetKind(), space.GetName(), archived(space)})
			},
		},
		&cobra.Command{
			Use:   "rename ID NAME",
			Short: "Rename a space",
			Args:  cobra.ExactArgs(2),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				renamed, err := session.api.Spaces.Rename(cmd.Context(), connect.NewRequest(&rookeryv1.SpaceServiceRenameRequest{
					Space: args[0],
					Name:  args[1],
				}))
				if err != nil {
					return err
				}
				space := renamed.Msg.GetSpace()
				return session.printer.line(space.GetId(), space.GetName())
			},
		},
		&cobra.Command{
			Use:   "archive ID",
			Short: "Archive a space, or bring it back with --off",
			Args:  cobra.ExactArgs(1),
			RunE: func(cmd *cobra.Command, args []string) error {
				off, err := cmd.Flags().GetBool("off")
				if err != nil {
					return err
				}
				session, err := open(cmd)
				if err != nil {
					return err
				}
				archivedSpace, err := session.api.Spaces.Archive(cmd.Context(), connect.NewRequest(&rookeryv1.SpaceServiceArchiveRequest{
					Space:    args[0],
					Archived: !off,
				}))
				if err != nil {
					return err
				}
				return session.printer.line(archivedSpace.Msg.GetSpace().GetId(), archived(archivedSpace.Msg.GetSpace()))
			},
		},
	)

	list, _, _ := command.Find([]string{"list"})
	if list != nil {
		list.Flags().Bool("archived", false, "include the archived spaces")
	}
	archive, _, _ := command.Find([]string{"archive"})
	if archive != nil {
		archive.Flags().Bool("off", false, "unarchive instead of archive")
	}
	return command
}

// archived is a space's state in a table.
func archived(space *rookeryv1.Space) string {
	if space.GetArchived() {
		return "archived"
	}
	if space.GetKind() == "" {
		return ""
	}
	return "active"
}
