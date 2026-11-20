package cli

import (
	"fmt"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// grantCommand is `rookery grant`: a level on a resource for a person, a group
// or everyone.
func grantCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "grant RESOURCE (--user EMAIL | --group NAME | --everyone) --level LEVEL",
		Short: "Give a person, a group or everyone a level on a resource",
		Long: `Give a person, a group or everyone a level on a resource.

A resource is a URI: space://ID, host://NAME, record://PLUGIN/COLLECTION/ID,
document://ID, plugin://ID, provisioner://NAME or rollout://ID. Levels are view,
use and manage, and a grant on a parent applies to its children.`,
		Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return changeGrant(cmd, args[0], true)
		},
	}
	addGrantFlags(command)
	return command
}

// revokeCommand is `rookery revoke`: takes a grant away.
func revokeCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "revoke RESOURCE (--user EMAIL | --group NAME | --everyone)",
		Short: "Take a grant away",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			return changeGrant(cmd, args[0], false)
		},
	}
	addGrantFlags(command)
	return command
}

// grantsCommand is `rookery grants`: the grants of a resource.
func grantsCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "grants RESOURCE",
		Short: "List the grants of a resource",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			listed, err := session.api.Access.ListGrants(cmd.Context(), connect.NewRequest(&rookeryv1.AccessServiceListGrantsRequest{
				Resource: args[0],
			}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetGrants()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetGrants()))
			for _, grant := range listed.Msg.GetGrants() {
				rows = append(rows, []string{grant.GetResource(), granteeName(grant.GetGrantee()), access.LevelName(grant.GetLevel())})
				messages = append(messages, grant)
			}
			return session.printer.list(messages, []string{"RESOURCE", "WHO", "LEVEL"}, rows)
		},
	}
}

// addGrantFlags adds who a grant is for and at which level.
func addGrantFlags(command *cobra.Command) {
	command.Flags().String("user", "", "a person, by email")
	command.Flags().String("group", "", "a group, by name")
	command.Flags().Bool("everyone", false, "every signed-in caller")
	command.Flags().String("level", "view", "the level: view, use or manage")
}

// changeGrant gives a grant, or takes it away.
func changeGrant(cmd *cobra.Command, resource string, give bool) error {
	who, err := granteeFlag(cmd)
	if err != nil {
		return err
	}
	session, err := open(cmd)
	if err != nil {
		return err
	}

	grantee, err := session.grantee(cmd, who)
	if err != nil {
		return err
	}

	if !give {
		if _, err := session.api.Access.Revoke(cmd.Context(), connect.NewRequest(&rookeryv1.AccessServiceRevokeRequest{
			Resource: resource,
			Grantee:  grantee,
		})); err != nil {
			return err
		}
		return session.printer.line(resource, granteeName(grantee), "revoked")
	}

	rawLevel, err := cmd.Flags().GetString("level")
	if err != nil {
		return err
	}
	level, err := access.ParseLevel(rawLevel)
	if err != nil {
		return err
	}
	if level != access.View && level != access.Use && level != access.Manage {
		return fmt.Errorf("cli: a grant carries view, use or manage, not %s", access.LevelName(level))
	}

	made, err := session.api.Access.Grant(cmd.Context(), connect.NewRequest(&rookeryv1.AccessServiceGrantRequest{
		Resource: resource,
		Grantee:  grantee,
		Level:    level,
	}))
	if err != nil {
		return err
	}
	return session.printer.line(made.Msg.GetGrant().GetResource(), granteeName(grantee), access.LevelName(made.Msg.GetGrant().GetLevel()))
}

// granteeFlag is who a grant names, from the flags.
func granteeFlag(cmd *cobra.Command) (string, error) {
	flags := cmd.Flags()
	user, _ := flags.GetString("user")
	group, _ := flags.GetString("group")
	everyone, _ := flags.GetBool("everyone")

	chosen := 0
	for _, set := range []bool{user != "", group != "", everyone} {
		if set {
			chosen++
		}
	}
	switch {
	case chosen == 0:
		return "", fmt.Errorf("cli: name who the grant is for: --user, --group or --everyone")
	case chosen > 1:
		return "", fmt.Errorf("cli: a grant is for one of --user, --group or --everyone")
	case everyone:
		return access.Everyone, nil
	case user != "":
		return "user:" + user, nil
	default:
		return "group:" + group, nil
	}
}

// grantee turns the flag's answer into the API's grantee.
func (s *session) grantee(cmd *cobra.Command, who string) (*rookeryv1.Grantee, error) {
	switch {
	case who == access.Everyone:
		return &rookeryv1.Grantee{Who: &rookeryv1.Grantee_Everyone{Everyone: true}}, nil
	case strings.HasPrefix(who, "user:"):
		user, err := s.findUser(cmd, strings.TrimPrefix(who, "user:"))
		if err != nil {
			return nil, err
		}
		return &rookeryv1.Grantee{Who: &rookeryv1.Grantee_UserId{UserId: user.GetId()}}, nil
	default:
		group, err := s.findGroup(cmd, strings.TrimPrefix(who, "group:"))
		if err != nil {
			return nil, err
		}
		return &rookeryv1.Grantee{Who: &rookeryv1.Grantee_GroupId{GroupId: group.GetId()}}, nil
	}
}

// granteeName is a grantee in a table.
func granteeName(grantee *rookeryv1.Grantee) string {
	switch who := grantee.GetWho().(type) {
	case *rookeryv1.Grantee_UserId:
		return "user:" + who.UserId
	case *rookeryv1.Grantee_GroupId:
		return "group:" + who.GroupId
	case *rookeryv1.Grantee_Everyone:
		if who.Everyone {
			return "everyone"
		}
	}
	return ""
}
