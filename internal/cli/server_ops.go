package cli

import (
	"bufio"
	"errors"
	"fmt"
	"io"
	"net/url"
	"strings"
)

// confirmReset asks for the word reset, and takes anything else for a no: the
// command exists to get past a schema digest this build does not know, and it
// throws away every row to do it.
func confirmReset(in io.Reader, out io.Writer, databaseURL string) error {
	_, _ = fmt.Fprintf(out, "This drops and recreates every schema of %s.\nType \"reset\" to continue: ", databaseName(databaseURL))

	answer, err := bufio.NewReader(in).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return fmt.Errorf("cli: reading the answer: %w", err)
	}
	if strings.TrimSpace(answer) != "reset" {
		return errors.New("cli: nothing was reset")
	}
	return nil
}

// databaseName is the host and database of a URL, without the credentials.
func databaseName(databaseURL string) string {
	parsed, err := url.Parse(databaseURL)
	if err != nil {
		return "the database"
	}
	return parsed.Host + parsed.Path
}
