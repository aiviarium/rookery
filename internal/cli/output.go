package cli

import (
	"fmt"
	"io"
	"strings"
	"text/tabwriter"

	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

// printer writes a command's result: a table for a person, protobuf JSON for a
// script.
type printer struct {
	out    io.Writer
	asJSON bool
}

// one prints a message, and the row a table shows for it.
func (p *printer) one(message proto.Message, headers []string, row []string) error {
	if p.asJSON {
		return p.message(message)
	}
	return p.table(headers, [][]string{row})
}

// list prints messages, with the rows a table shows for them.
func (p *printer) list(messages []proto.Message, headers []string, rows [][]string) error {
	if !p.asJSON {
		return p.table(headers, rows)
	}
	if len(messages) == 0 {
		_, err := fmt.Fprintln(p.out, "[]")
		return err
	}
	options := protojson.MarshalOptions{Multiline: true, Indent: "  "}
	parts := make([]string, 0, len(messages))
	for _, message := range messages {
		raw, err := options.Marshal(message)
		if err != nil {
			return fmt.Errorf("cli: printing the result: %w", err)
		}
		parts = append(parts, string(raw))
	}
	_, err := fmt.Fprintf(p.out, "[%s]\n", strings.Join(parts, ",\n"))
	return err
}

// message prints one message as JSON.
func (p *printer) message(message proto.Message) error {
	options := protojson.MarshalOptions{Multiline: true, Indent: "  "}
	raw, err := options.Marshal(message)
	if err != nil {
		return fmt.Errorf("cli: printing the result: %w", err)
	}
	_, err = fmt.Fprintln(p.out, string(raw))
	return err
}

// table prints rows under their headers, aligned.
func (p *printer) table(headers []string, rows [][]string) error {
	writer := tabwriter.NewWriter(p.out, 0, 0, 2, ' ', 0)
	if _, err := fmt.Fprintln(writer, strings.Join(headers, "\t")); err != nil {
		return err
	}
	for _, row := range rows {
		if _, err := fmt.Fprintln(writer, strings.Join(row, "\t")); err != nil {
			return err
		}
	}
	return writer.Flush()
}

// line prints one plain line, for a command whose answer is a URL or a token.
func (p *printer) line(values ...string) error {
	if p.asJSON {
		return fmt.Errorf("cli: this command prints one value and has no JSON output")
	}
	_, err := fmt.Fprintln(p.out, strings.Join(values, "\t"))
	return err
}
