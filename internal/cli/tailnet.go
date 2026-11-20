package cli

import (
	"context"
	"errors"
	"fmt"
	"io"
	"strconv"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"
	"golang.org/x/sync/errgroup"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
)

func tailnetCommand() *cobra.Command {
	root := &cobra.Command{Use: "tailnet", Short: "Configure and inspect the tailnet"}
	root.AddCommand(&cobra.Command{
		Use: "status", Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			s, err := open(cmd)
			if err != nil {
				return err
			}
			out, err := s.api.Tailnet.Status(cmd.Context(), connect.NewRequest(&rookeryv1.TailnetServiceStatusRequest{}))
			if err != nil {
				return err
			}
			state := "unconfigured"
			if out.Msg.GetConfigured() {
				state = "offline"
			}
			if out.Msg.GetOnline() {
				state = "online"
			}
			return s.printer.one(out.Msg, []string{"STATE", "CONTROL", "TAG", "ADDRESSES"},
				[]string{state, out.Msg.GetControlUrl(), out.Msg.GetTag(), strings.Join(out.Msg.GetAddresses(), ", ")})
		},
	}, &cobra.Command{
		Use: "devices", Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			s, err := open(cmd)
			if err != nil {
				return err
			}
			out, err := s.api.Tailnet.ListDevices(cmd.Context(), connect.NewRequest(&rookeryv1.TailnetServiceListDevicesRequest{}))
			if err != nil {
				return err
			}
			if s.printer.asJSON {
				return s.printer.message(out.Msg)
			}
			var rows [][]string
			for _, device := range out.Msg.GetDevices() {
				rows = append(rows, []string{device.GetId(), device.GetHostname(), strings.Join(device.GetAddresses(), ", "), strconv.FormatBool(device.GetOnline())})
			}
			return s.printer.table([]string{"ID", "HOSTNAME", "ADDRESSES", "ONLINE"}, rows)
		},
	})
	var control, tag string
	var stdin bool
	configure := &cobra.Command{
		Use: "configure", Args: cobra.NoArgs,
		RunE: func(cmd *cobra.Command, _ []string) error {
			if !stdin {
				return errors.New("tailnet configuration requires --api-key-stdin")
			}
			raw, err := io.ReadAll(io.LimitReader(cmd.InOrStdin(), 8193))
			if err != nil {
				return fmt.Errorf("reading the API key: %w", err)
			}
			key := strings.TrimSpace(string(raw))
			if key == "" || len(raw) > 8192 {
				return errors.New("invalid API key length")
			}
			s, err := open(cmd)
			if err != nil {
				return err
			}
			out, err := s.api.Tailnet.Configure(cmd.Context(), connect.NewRequest(&rookeryv1.TailnetServiceConfigureRequest{
				ControlUrl: control, Tag: tag, ApiKey: key,
			}))
			if err != nil {
				return err
			}
			if s.printer.asJSON {
				return s.printer.message(out.Msg)
			}
			return s.printer.line("tailnet configured")
		},
	}
	configure.Flags().StringVar(&control, "control", "", "HTTPS origin of the control server")
	configure.Flags().StringVar(&tag, "tag", "", "tag for managed hosts")
	configure.Flags().BoolVar(&stdin, "api-key-stdin", false, "read the control API key from standard input")
	root.AddCommand(configure)
	return root
}

// dialCommand is the byte-clean SSH ProxyCommand through the local socket.
func dialCommand() *cobra.Command {
	return &cobra.Command{
		Use: "dial HOST PORT", Short: "Open a byte stream through the tailnet", Args: cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			port, err := strconv.ParseUint(args[1], 10, 16)
			if err != nil || port == 0 {
				return fmt.Errorf("invalid port %q", args[1])
			}
			s, err := open(cmd)
			if err != nil {
				return err
			}
			return s.dialTailnet(cmd, args[0], uint32(port))
		},
	}
}

func (s *session) dialTailnet(cmd *cobra.Command, host string, port uint32) error {
	ctx, cancel := context.WithCancel(cmd.Context())
	defer cancel()
	group, groupCtx := errgroup.WithContext(ctx)
	stream := s.api.Local.Dial(groupCtx)
	defer func() { _ = stream.CloseResponse() }()
	if err := stream.Send(&rookeryv1.LocalServiceDialRequest{Host: host, Port: port}); err != nil {
		if errors.Is(err, io.EOF) {
			_, err = stream.Receive()
		}
		return err
	}
	input := cmd.InOrStdin()
	stopRead := context.AfterFunc(groupCtx, func() {
		if closer, ok := input.(io.Closer); ok {
			_ = closer.Close()
		}
	})
	defer stopRead()

	group.Go(func() error {
		defer cancel()
		for {
			response, err := stream.Receive()
			if errors.Is(err, io.EOF) {
				return nil
			}
			if err != nil {
				return err
			}
			if _, err := cmd.OutOrStdout().Write(response.GetData()); err != nil {
				return err
			}
		}
	})
	group.Go(func() error {
		defer func() { _ = stream.CloseRequest() }()
		buffer := make([]byte, 64<<10)
		for {
			n, err := input.Read(buffer)
			if n != 0 {
				if err := stream.Send(&rookeryv1.LocalServiceDialRequest{Data: buffer[:n]}); err != nil {
					if errors.Is(err, io.EOF) || groupCtx.Err() != nil {
						return nil
					}
					return err
				}
			}
			if errors.Is(err, io.EOF) || groupCtx.Err() != nil {
				return nil
			}
			if err != nil {
				return err
			}
		}
	})
	return group.Wait()
}
