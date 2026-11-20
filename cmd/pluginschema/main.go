// Command pluginschema writes the JSON Schema of a plugin's manifest, which is
// generated from internal/plugins' Go types. `make gen` runs it; the file it
// writes is checked in, and a test in internal/plugins fails when the two
// disagree.
package main

import (
	"flag"
	"fmt"
	"os"

	"github.com/aiviarium/rookery/internal/plugins"
)

func main() {
	output := flag.String("o", "schemas/plugin.schema.json", "where to write the schema")
	flag.Parse()

	if err := run(*output); err != nil {
		fmt.Fprintln(os.Stderr, "pluginschema:", err)
		os.Exit(1)
	}
}

func run(output string) error {
	schema, err := plugins.Schema()
	if err != nil {
		return err
	}
	//nolint:gosec // a schema is a file everyone may read.
	if err := os.WriteFile(output, schema, 0o644); err != nil {
		return fmt.Errorf("writing %s: %w", output, err)
	}
	return nil
}
