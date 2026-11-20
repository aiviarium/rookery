package cli

import (
	"fmt"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// groupCommand is `rookery group`: groups, their members and their claims.
func groupCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "group",
		Short: "List groups, make them, fill them, bind them to a claim",
	}

	command.AddCommand(
		&cobra.Command{
			Use:   "list",
			Short: "List groups with their members and claims",
			RunE: func(cmd *cobra.Command, _ []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				listed, err := session.api.Groups.List(cmd.Context(), connect.NewRequest(&rookeryv1.GroupServiceListRequest{}))
				if err != nil {
					return err
				}
				rows := make([][]string, 0, len(listed.Msg.GetGroups()))
				messages := make([]proto.Message, 0, len(listed.Msg.GetGroups()))
				for _, group := range listed.Msg.GetGroups() {
					rows = append(rows, []string{group.GetId(), group.GetName(), memberEmails(group), claimBindings(group)})
					messages = append(messages, group)
				}
				return session.printer.list(messages, []string{"ID", "NAME", "MEMBERS", "CLAIMS"}, rows)
			},
		},
		&cobra.Command{
			Use:   "create NAME",
			Short: "Make a group",
			Args:  cobra.ExactArgs(1),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				made, err := session.api.Groups.Create(cmd.Context(), connect.NewRequest(&rookeryv1.GroupServiceCreateRequest{Name: args[0]}))
				if err != nil {
					return err
				}
				return session.printer.one(made.Msg.GetGroup(), []string{"ID", "NAME"}, []string{made.Msg.GetGroup().GetId(), made.Msg.GetGroup().GetName()})
			},
		},
		&cobra.Command{
			Use:   "add NAME EMAIL",
			Short: "Put a person in a group",
			Args:  cobra.ExactArgs(2),
			RunE: func(cmd *cobra.Command, args []string) error {
				return changeMembership(cmd, args[0], args[1], true)
			},
		},
		&cobra.Command{
			Use:   "remove NAME EMAIL",
			Short: "Take a person out of a group",
			Args:  cobra.ExactArgs(2),
			RunE: func(cmd *cobra.Command, args []string) error {
				return changeMembership(cmd, args[0], args[1], false)
			},
		},
		&cobra.Command{
			Use:   "bind NAME CLAIM [VALUE]",
			Short: "Join the holders of an OIDC claim value to a group; VALUE defaults to the group's name",
			Args:  cobra.RangeArgs(2, 3),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				group, err := session.findGroup(cmd, args[0])
				if err != nil {
					return err
				}
				value := group.GetName()
				if len(args) == 3 {
					value = args[2]
				}
				bound, err := session.api.Groups.BindClaim(cmd.Context(), connect.NewRequest(&rookeryv1.GroupServiceBindClaimRequest{
					Group: group.GetId(),
					Claim: args[1],
					Value: value,
				}))
				if err != nil {
					return err
				}
				return session.printer.line(bound.Msg.GetGroup().GetName(), args[1], value)
			},
		},
	)
	return command
}

// changeMembership adds a person to a group, or removes them.
func changeMembership(cmd *cobra.Command, groupName, email string, add bool) error {
	session, err := open(cmd)
	if err != nil {
		return err
	}
	group, err := session.findGroup(cmd, groupName)
	if err != nil {
		return err
	}

	if add {
		added, err := session.api.Groups.AddMember(cmd.Context(), connect.NewRequest(&rookeryv1.GroupServiceAddMemberRequest{
			Group: group.GetId(),
			Email: email,
		}))
		if err != nil {
			return err
		}
		return session.printer.line(added.Msg.GetGroup().GetName(), email, "added")
	}

	removed, err := session.api.Groups.RemoveMember(cmd.Context(), connect.NewRequest(&rookeryv1.GroupServiceRemoveMemberRequest{
		Group: group.GetId(),
		Email: email,
	}))
	if err != nil {
		return err
	}
	return session.printer.line(removed.Msg.GetGroup().GetName(), email, "removed")
}

// findGroup is the group a name or an id names, which must be there.
func (s *session) findGroup(cmd *cobra.Command, name string) (*rookeryv1.Group, error) {
	listed, err := s.api.Groups.List(cmd.Context(), connect.NewRequest(&rookeryv1.GroupServiceListRequest{}))
	if err != nil {
		return nil, err
	}
	for _, group := range listed.Msg.GetGroups() {
		if group.GetId() == name || strings.EqualFold(group.GetName(), name) {
			return group, nil
		}
	}
	return nil, fmt.Errorf("cli: there is no group called %s", name)
}

// memberEmails is a group's members, comma-separated.
func memberEmails(group *rookeryv1.Group) string {
	emails := make([]string, 0, len(group.GetMembers()))
	for _, member := range group.GetMembers() {
		emails = append(emails, member.GetEmail())
	}
	return strings.Join(emails, ",")
}

// claimBindings is a group's claim bindings, comma-separated.
func claimBindings(group *rookeryv1.Group) string {
	bindings := make([]string, 0, len(group.GetClaims()))
	for _, claim := range group.GetClaims() {
		bindings = append(bindings, claim.GetClaim()+"="+claim.GetValue())
	}
	return strings.Join(bindings, ",")
}
