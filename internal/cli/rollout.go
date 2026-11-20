package cli

import (
	"fmt"
	"strconv"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/types/known/durationpb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

func rolloutCommand() *cobra.Command {
	root := &cobra.Command{Use: "rollout", Short: "Plan and run changes across hosts"}
	root.AddCommand(rolloutSelectionCommand(false), rolloutSelectionCommand(true), rolloutListCommand())
	for _, operation := range []string{"show", "pause", "resume", "cancel"} {
		root.AddCommand(rolloutStateCommand(operation))
	}
	return root
}

func rolloutSelectionCommand(start bool) *cobra.Command {
	var hosts []string
	var labels, action, batch string
	var behind bool
	var failures, canary int32
	var wait time.Duration
	name := "plan"
	if start {
		name = "start"
	}
	cmd := &cobra.Command{
		Use: name, Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			selection := &rookeryv1.RolloutSelection{Hosts: hosts, Labels: labels, Behind: behind}
			kind := "host.apply"
			var args []byte
			var err error
			if action != "" {
				kind = "host.action"
				args, err = cborx.Marshal(map[string]any{"action": action})
				if err != nil {
					return err
				}
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			if !start {
				out, err := session.api.Rollouts.Plan(cmd.Context(), connect.NewRequest(&rookeryv1.RolloutServicePlanRequest{
					Kind: kind, Selection: selection, ArgsCbor: args,
				}))
				if err != nil {
					return err
				}
				if session.printer.asJSON {
					return session.printer.message(out.Msg)
				}
				return printRolloutHosts(session.printer, out.Msg.GetHosts(), out.Msg.GetSkipped())
			}
			strategy, err := rolloutStrategy(batch, failures, canary, wait)
			if err != nil {
				return err
			}
			out, err := session.api.Rollouts.Start(cmd.Context(), connect.NewRequest(&rookeryv1.RolloutServiceStartRequest{
				Kind: kind, Selection: selection, ArgsCbor: args, Strategy: strategy,
			}))
			if err != nil {
				return err
			}
			return session.followRollout(cmd, out.Msg.GetRollout().GetId())
		},
	}
	cmd.Flags().StringSliceVar(&hosts, "hosts", nil, "select these comma-separated hosts")
	cmd.Flags().StringVar(&labels, "label", "", "select hosts by a label expression")
	cmd.Flags().BoolVar(&behind, "behind", false, "select hosts whose profile has changed")
	cmd.Flags().StringVar(&action, "action", "", "run this action instead of applying")
	if start {
		cmd.Flags().StringVar(&batch, "batch", "1", "batch size as a count or percentage")
		cmd.Flags().Int32Var(&failures, "max-failures", 1, "pause after this many failures")
		cmd.Flags().Int32Var(&canary, "canary", 0, "pause after the canary hosts finish")
		cmd.Flags().DurationVar(&wait, "wait", 0, "wait between batches")
	}
	return cmd
}

func rolloutStrategy(batch string, failures, canary int32, wait time.Duration) (*rookeryv1.RolloutStrategy, error) {
	count, percent := strings.CutSuffix(batch, "%")
	size, err := strconv.ParseInt(count, 10, 32)
	if err != nil || size <= 0 || percent && size > 100 {
		return nil, fmt.Errorf("invalid batch size %q", batch)
	}
	strategy := &rookeryv1.RolloutStrategy{FailuresAllowed: failures, Canary: canary, Pause: durationpb.New(wait)}
	if percent {
		strategy.BatchPercent = int32(size)
	} else {
		strategy.BatchSize = int32(size)
	}
	return strategy, nil
}

func rolloutListCommand() *cobra.Command {
	return &cobra.Command{
		Use: "list", Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			out, err := session.api.Rollouts.List(cmd.Context(), connect.NewRequest(&rookeryv1.RolloutServiceListRequest{}))
			if err != nil {
				return err
			}
			if session.printer.asJSON {
				return session.printer.message(out.Msg)
			}
			var rows [][]string
			for _, rollout := range out.Msg.GetRollouts() {
				rows = append(rows, []string{rollout.GetId(), rollout.GetKind(), rollout.GetState()})
			}
			return session.printer.table([]string{"ID", "KIND", "STATE"}, rows)
		},
	}
}

func rolloutStateCommand(operation string) *cobra.Command {
	var follow bool
	cmd := &cobra.Command{
		Use: operation + " ID", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			id := args[0]
			ctx, api := cmd.Context(), session.api.Rollouts
			var rollout *rookeryv1.Rollout
			switch operation {
			case "show":
				if follow {
					return session.followRollout(cmd, id)
				}
				out, err := api.Get(ctx, connect.NewRequest(&rookeryv1.RolloutServiceGetRequest{Rollout: id}))
				if err != nil {
					return err
				}
				rollout = out.Msg.GetRollout()
			case "pause":
				out, err := api.Pause(ctx, connect.NewRequest(&rookeryv1.RolloutServicePauseRequest{Rollout: id}))
				if err != nil {
					return err
				}
				rollout = out.Msg.GetRollout()
			case "resume":
				if _, err := api.Resume(ctx, connect.NewRequest(&rookeryv1.RolloutServiceResumeRequest{Rollout: id})); err != nil {
					return err
				}
				return session.followRollout(cmd, id)
			case "cancel":
				out, err := api.Cancel(ctx, connect.NewRequest(&rookeryv1.RolloutServiceCancelRequest{Rollout: id}))
				if err != nil {
					return err
				}
				rollout = out.Msg.GetRollout()
			}
			return session.printRollout(rollout)
		},
	}
	if operation == "show" {
		cmd.Flags().BoolVar(&follow, "follow", false, "watch progress")
	}
	return cmd
}

func (s *session) followRollout(cmd *cobra.Command, id string) error {
	stream, err := s.api.Rollouts.Watch(cmd.Context(), connect.NewRequest(&rookeryv1.RolloutServiceWatchRequest{Rollout: id}))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()
	for stream.Receive() {
		rollout := stream.Msg().GetRollout()
		if err := s.printRollout(rollout); err != nil {
			return err
		}
		switch state := rollout.GetState(); state {
		case "succeeded":
			return nil
		case "paused", "failed", "cancelled":
			if reason := rollout.GetPausedBecause(); reason != "" {
				return fmt.Errorf("roll-out %s %s: %s", id, state, reason)
			}
			return fmt.Errorf("roll-out %s %s", id, state)
		}
	}
	return stream.Err()
}

func (s *session) printRollout(rollout *rookeryv1.Rollout) error {
	if s.printer.asJSON {
		return s.printer.message(rollout)
	}
	if _, err := fmt.Fprintln(s.printer.out, rollout.GetId(), rollout.GetKind(), rollout.GetState()); err != nil {
		return err
	}
	return printRolloutHosts(s.printer, rollout.GetHosts(), rollout.GetSkipped())
}

func printRolloutHosts(printer *printer, hosts, skipped []*rookeryv1.RolloutHost) error {
	var rows [][]string
	for _, group := range [][]*rookeryv1.RolloutHost{hosts, skipped} {
		for _, host := range group {
			rows = append(rows, []string{host.GetHost(), host.GetState(), firstNonEmpty(host.GetError(), host.GetReason())})
		}
	}
	return printer.table([]string{"HOST", "STATE", "DETAIL"}, rows)
}
