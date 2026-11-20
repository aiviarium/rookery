package cli

import (
	"fmt"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/filter"
)

func dataCommand() *cobra.Command {
	root := &cobra.Command{Use: "data", Short: "Read and follow datastore records"}
	get := &cobra.Command{
		Use: "get PLUGIN/COLLECTION ID", Args: cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			s, err := open(cmd)
			if err != nil {
				return err
			}
			response, err := s.api.Data.Get(cmd.Context(), connect.NewRequest(&rookeryv1.GetRequest{Collection: args[0], Id: args[1]}))
			if err != nil {
				return err
			}
			if s.printer.asJSON {
				return s.printer.message(response.Msg)
			}
			return printRecord(s.printer, response.Msg.GetRecordCbor())
		},
	}
	root.AddCommand(get, dataQueryCommand(false), dataQueryCommand(true))
	return root
}

func dataQueryCommand(live bool) *cobra.Command {
	var where, space, cursor string
	var limit int32
	name := "find"
	if live {
		name = "live"
	}
	command := &cobra.Command{
		Use: name + " PLUGIN/COLLECTION", Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			encoded, err := filter.EncodeText(where)
			if err != nil {
				return err
			}
			s, err := open(cmd)
			if err != nil {
				return err
			}
			if live {
				return s.liveData(cmd, &rookeryv1.LiveRequest{Collection: args[0], FilterCbor: encoded, Space: space, Limit: limit})
			}
			response, err := s.api.Data.Find(cmd.Context(), connect.NewRequest(&rookeryv1.FindRequest{
				Collection: args[0], FilterCbor: encoded, Space: space, Limit: limit, Cursor: cursor,
			}))
			if err != nil {
				return err
			}
			if s.printer.asJSON {
				return s.printer.message(response.Msg)
			}
			for _, record := range response.Msg.GetRecordsCbor() {
				if err := printRecord(s.printer, record); err != nil {
					return err
				}
			}
			if next := response.Msg.GetCursor(); next != "" {
				_, err := fmt.Fprintln(cmd.ErrOrStderr(), "Next cursor:", next)
				return err
			}
			return nil
		},
	}
	command.Flags().StringVar(&where, "where", "", "filter expression")
	command.Flags().StringVar(&space, "space", "", "space to query")
	command.Flags().Int32Var(&limit, "limit", 0, "maximum number of records in the initial result")
	if !live {
		command.Flags().StringVar(&cursor, "cursor", "", "cursor from a previous find")
	}
	return command
}

func (s *session) liveData(cmd *cobra.Command, request *rookeryv1.LiveRequest) error {
	stream, err := s.api.Data.Live(cmd.Context(), connect.NewRequest(request))
	if err != nil {
		return err
	}
	defer func() { _ = stream.Close() }()
	for stream.Receive() {
		event := stream.Msg()
		if s.printer.asJSON {
			if err := s.printer.message(event); err != nil {
				return err
			}
			continue
		}
		if _, err := fmt.Fprintln(s.printer.out, event.GetKind(), event.GetResource()); err != nil {
			return err
		}
		if err := printRecord(s.printer, event.GetRecordCbor()); err != nil {
			return err
		}
	}
	return stream.Err()
}

func printRecord(p *printer, encoded []byte) error {
	if len(encoded) == 0 {
		return nil
	}
	var record map[string]any
	if err := cborx.Unmarshal(encoded, &record); err != nil {
		return err
	}
	_, err := fmt.Fprintln(p.out, record)
	return err
}
