package main

import (
	"bufio"
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"net/url"
	"os"
	"strings"

	"github.com/aiviarium/rookery/internal/store"
)

// serverCommand runs the commands of `rookery server`.
func serverCommand(ctx context.Context, args []string) error {
	if len(args) == 0 {
		return errors.New("usage: rookery server reset")
	}

	switch args[0] {
	case "reset":
		return reset(ctx, args[1:])
	default:
		return fmt.Errorf("unknown command \"server %s\"", args[0])
	}
}

// reset drops and recreates the database's schemas, after a confirmation: the
// command exists to get past a schema digest that this build does not know, and
// it throws away every row to do it.
func reset(ctx context.Context, args []string) error {
	flags := flag.NewFlagSet("server reset", flag.ContinueOnError)
	assumeYes := flags.Bool("yes", false, "do not ask for a confirmation")
	if err := flags.Parse(args); err != nil {
		return err
	}

	databaseURL := os.Getenv("ROOKERY_DATABASE_URL")
	if databaseURL == "" {
		return errors.New("ROOKERY_DATABASE_URL is not set")
	}

	if !*assumeYes {
		if err := confirm(os.Stdin, os.Stdout, databaseURL); err != nil {
			return err
		}
	}

	opened, err := store.Open(ctx, store.Config{URL: databaseURL})
	if err != nil {
		return err
	}
	defer opened.Close()

	if err := opened.Reset(ctx); err != nil {
		return err
	}

	fmt.Println("dropped and recreated the schemas of", databaseName(databaseURL))
	return nil
}

// confirm asks for the word reset, and takes anything else for a no.
func confirm(in io.Reader, out io.Writer, databaseURL string) error {
	_, _ = fmt.Fprintf(out, "This drops and recreates every schema of %s.\nType \"reset\" to continue: ", databaseName(databaseURL))

	answer, err := bufio.NewReader(in).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return fmt.Errorf("reading the answer: %w", err)
	}
	if strings.TrimSpace(answer) != "reset" {
		return errors.New("nothing was reset")
	}
	return nil
}

// databaseName is the host and database of the URL, without the credentials.
func databaseName(databaseURL string) string {
	parsed, err := url.Parse(databaseURL)
	if err != nil {
		return "the database"
	}
	return parsed.Host + parsed.Path
}
