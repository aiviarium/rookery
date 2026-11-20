package cli

import (
	"context"
	"fmt"
	"io"
	"time"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"google.golang.org/protobuf/proto"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

// jobCommand is `rookery job`: the jobs of the deployment.
func jobCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "job",
		Short: "List jobs, show one, read its log, cancel it, retry it",
	}

	list := &cobra.Command{
		Use:   "list",
		Short: "List jobs, newest first",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			kind, _ := cmd.Flags().GetString("kind")
			state, _ := cmd.Flags().GetString("state")
			listed, err := session.api.Jobs.List(cmd.Context(), connect.NewRequest(&rookeryv1.JobServiceListRequest{
				Kind:  kind,
				State: state,
			}))
			if err != nil {
				return err
			}
			rows := make([][]string, 0, len(listed.Msg.GetJobs()))
			messages := make([]proto.Message, 0, len(listed.Msg.GetJobs()))
			for _, job := range listed.Msg.GetJobs() {
				created := ""
				if job.GetCreatedAt() != nil {
					created = job.GetCreatedAt().AsTime().Format(time.RFC3339)
				}
				rows = append(rows, []string{job.GetId(), job.GetKind(), job.GetState(), job.GetResource(), created})
				messages = append(messages, job)
			}
			return session.printer.list(messages, []string{"ID", "KIND", "STATE", "RESOURCE", "CREATED"}, rows)
		},
	}
	list.Flags().String("kind", "", "only jobs of this kind")
	list.Flags().String("state", "", "only jobs in this state")

	show := &cobra.Command{
		Use:   "show ID",
		Short: "Show one job",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			found, err := session.api.Jobs.Get(cmd.Context(), connect.NewRequest(&rookeryv1.JobServiceGetRequest{Job: args[0]}))
			if err != nil {
				return err
			}
			return session.printer.message(found.Msg.GetJob())
		},
	}

	logs := &cobra.Command{
		Use:   "logs ID [--follow]",
		Short: "Read a job's log, and with --follow wait for the end",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			follow, _ := cmd.Flags().GetBool("follow")
			return followJob(cmd.Context(), cmd.OutOrStdout(), session, args[0], follow)
		},
	}
	logs.Flags().BoolP("follow", "f", false, "stream the log and wait for the job to end")

	cancel := &cobra.Command{
		Use:   "cancel ID",
		Short: "Ask a job to stop",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			job, err := session.api.Jobs.Cancel(cmd.Context(), connect.NewRequest(&rookeryv1.JobServiceCancelRequest{Job: args[0]}))
			if err != nil {
				return err
			}
			return session.printer.line(job.Msg.GetJob().GetId(), job.Msg.GetJob().GetState())
		},
	}

	retry := &cobra.Command{
		Use:   "retry ID",
		Short: "Put a job back in the queue",
		Args:  cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			job, err := session.api.Jobs.Retry(cmd.Context(), connect.NewRequest(&rookeryv1.JobServiceRetryRequest{Job: args[0]}))
			if err != nil {
				return err
			}
			return session.printer.line(job.Msg.GetJob().GetId(), job.Msg.GetJob().GetState())
		},
	}

	command.AddCommand(list, show, logs, cancel, retry)
	return command
}

// followJob streams a job's log and, with follow, its end.
func followJob(ctx context.Context, out io.Writer, session *session, jobID string, follow bool) error {
	stream, err := session.api.Jobs.Follow(ctx, connect.NewRequest(&rookeryv1.JobServiceFollowRequest{Job: jobID, From: 0}))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()

	var last any
	for stream.Receive() {
		message := stream.Msg()
		switch event := message.GetEvent().(type) {
		case *rookeryv1.JobServiceFollowResponse_Line:
			if _, err := fmt.Fprintln(out, event.Line.GetLine()); err != nil {
				return err
			}
		case *rookeryv1.JobServiceFollowResponse_Job:
			if _, err := fmt.Fprintf(out, "%s %s\n", event.Job.GetId(), event.Job.GetState()); err != nil {
				return err
			}
			last = event.Job.GetState()
			if !follow {
				return nil
			}
		}
	}
	if err := stream.Err(); err != nil {
		return err
	}
	if last == nil {
		return nil
	}
	return nil
}

// scheduleCommand is `rookery schedule`: the deployment's schedules.
func scheduleCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "schedule",
		Short: "List schedules and run one now",
	}
	command.AddCommand(
		&cobra.Command{
			Use:   "list",
			Short: "List schedules",
			RunE: func(cmd *cobra.Command, _ []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				listed, err := session.api.Schedules.List(cmd.Context(), connect.NewRequest(&rookeryv1.ScheduleServiceListRequest{}))
				if err != nil {
					return err
				}
				rows := make([][]string, 0, len(listed.Msg.GetSchedules()))
				messages := make([]proto.Message, 0, len(listed.Msg.GetSchedules()))
				for _, schedule := range listed.Msg.GetSchedules() {
					next := ""
					if schedule.GetNextRunAt() != nil {
						next = schedule.GetNextRunAt().AsTime().Format(time.RFC3339)
					}
					rows = append(rows, []string{schedule.GetId(), schedule.GetName(), schedule.GetKind(), schedule.GetCron(), schedule.GetTimezone(), next})
					messages = append(messages, schedule)
				}
				return session.printer.list(messages, []string{"ID", "NAME", "KIND", "CRON", "TZ", "NEXT"}, rows)
			},
		},
		&cobra.Command{
			Use:   "run ID",
			Short: "Run a schedule's job now",
			Args:  cobra.ExactArgs(1),
			RunE: func(cmd *cobra.Command, args []string) error {
				session, err := open(cmd)
				if err != nil {
					return err
				}
				made, err := session.api.Schedules.RunNow(cmd.Context(), connect.NewRequest(&rookeryv1.ScheduleServiceRunNowRequest{Schedule: args[0]}))
				if err != nil {
					return err
				}
				return session.printer.line(made.Msg.GetJob())
			},
		},
	)
	return command
}
