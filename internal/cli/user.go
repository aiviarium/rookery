package cli

import (
	"fmt"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// userCommand is `rookery user`: the people of the deployment.
func userCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "user",
		Short: "List people, make sign-in links, give or take the admin flag",
	}

	list := &cobra.Command{
		Use:   "list",
		Short: "List people",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			listed, err := session.api.Users.List(cmd.Context(), connect.NewRequest(&rookeryv1.UserServiceListRequest{}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetUsers()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetUsers()))
			for _, user := range listed.Msg.GetUsers() {
				var flags []string
				if user.GetAdmin() {
					flags = append(flags, "admin")
				}
				if user.GetDisabled() {
					flags = append(flags, "disabled")
				}
				rows = append(rows, []string{user.GetId(), user.GetEmail(), user.GetName(), strings.Join(flags, " ")})
				messages = append(messages, user)
			}
			return session.printer.list(messages, []string{"ID", "EMAIL", "NAME", "FLAGS"}, rows)
		},
	}

	link := &cobra.Command{
		Use:   "link EMAIL",
		Short: "Make a one-time sign-in link for an email",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			made, err := session.api.Users.CreateLink(cmd.Context(), connect.NewRequest(&rookeryv1.UserServiceCreateLinkRequest{Email: args[0]}))
			if err != nil {
				return err
			}
			return session.printer.line(made.Msg.GetLink().GetUrl())
		},
	}

	admin := &cobra.Command{
		Use:   "admin EMAIL",
		Short: "Give a person the admin flag, or take it away with --off",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return setUserFlag(cmd, args[0], "admin", true)
		},
	}
	admin.Flags().Bool("off", false, "take the flag away instead of giving it")

	disable := &cobra.Command{
		Use:   "disable EMAIL",
		Short: "Stop a person signing in, or let them again with --off",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return setUserFlag(cmd, args[0], "disabled", true)
		},
	}
	disable.Flags().Bool("off", false, "take the flag away instead of giving it")

	command.AddCommand(list, link, admin, disable)
	return command
}

// setUserFlag turns a user's admin or disabled flag on or off, with --off for the
// other way round.
func setUserFlag(cmd *cobra.Command, email, flag string, on bool) error {
	if off, err := cmd.Flags().GetBool("off"); err == nil && off {
		on = !on
	}
	session, err := open(cmd)
	if err != nil {
		return err
	}
	user, err := session.findUser(cmd, email)
	if err != nil {
		return err
	}

	switch flag {
	case "admin":
		if _, err := session.api.Users.SetAdmin(cmd.Context(), connect.NewRequest(&rookeryv1.UserServiceSetAdminRequest{User: user.GetId(), Admin: on})); err != nil {
			return err
		}
	case "disabled":
		if _, err := session.api.Users.SetDisabled(cmd.Context(), connect.NewRequest(&rookeryv1.UserServiceSetDisabledRequest{User: user.GetId(), Disabled: on})); err != nil {
			return err
		}
	default:
		return fmt.Errorf("cli: there is no flag %q", flag)
	}

	state := "off"
	if on {
		state = "on"
	}
	return session.printer.line(user.GetEmail(), flag, state)
}

// findUser is the user with an email, which must be there.
func (s *session) findUser(cmd *cobra.Command, email string) (*rookeryv1.User, error) {
	if email == "" {
		return nil, fmt.Errorf("cli: an email is required")
	}
	found, err := s.api.Users.Search(cmd.Context(), connect.NewRequest(&rookeryv1.UserServiceSearchRequest{Query: email, Limit: 10}))
	if err != nil {
		return nil, err
	}
	for _, user := range found.Msg.GetUsers() {
		if strings.EqualFold(user.GetEmail(), strings.TrimSpace(email)) {
			return user, nil
		}
	}
	return nil, fmt.Errorf("cli: there is no user with the email %s", email)
}
