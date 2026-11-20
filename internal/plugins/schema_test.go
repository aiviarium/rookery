package plugins_test

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/aiviarium/rookery/internal/plugins"
)

// TestSchemaIsCheckedIn fails when the schema in schemas/ is not what the Go
// types say: `make gen` writes it, and it is checked in for editors.
func TestSchemaIsCheckedIn(t *testing.T) {
	t.Parallel()

	generated, err := plugins.Schema()
	if err != nil {
		t.Fatalf("Schema() = %v", err)
	}
	checkedIn, err := os.ReadFile(filepath.Join("..", "..", "schemas", "plugin.schema.json"))
	if err != nil {
		t.Fatalf("reading schemas/plugin.schema.json: %v", err)
	}
	if string(generated) != string(checkedIn) {
		t.Fatalf("schemas/plugin.schema.json is stale: run `make gen`")
	}

	// And it is a schema an editor can read.
	var schema map[string]any
	if err := json.Unmarshal(checkedIn, &schema); err != nil {
		t.Fatalf("the schema is not JSON: %v", err)
	}
	if schema["$schema"] == nil || schema["$id"] == nil {
		t.Errorf("the schema has no $schema or $id")
	}
	properties, _ := schema["properties"].(map[string]any)
	for _, name := range []string{
		"id", "version", "name", "requires", "service", "collections", "hostFeatures", "contributes",
	} {
		if _, ok := properties[name]; !ok {
			t.Errorf("the schema has no %s", name)
		}
	}
	required, _ := schema["required"].([]any)
	if len(required) != 3 {
		t.Errorf("the schema requires %v, want id, version and name", required)
	}
}

func TestSchemaRefusesUnknownKeys(t *testing.T) {
	t.Parallel()

	generated, err := plugins.Schema()
	if err != nil {
		t.Fatalf("Schema() = %v", err)
	}
	var schema struct {
		Additional any `json:"additionalProperties"`
	}
	if err := json.Unmarshal(generated, &schema); err != nil {
		t.Fatalf("decoding the schema: %v", err)
	}
	if schema.Additional != false {
		t.Errorf("the manifest's schema allows keys Rookery does not know: %v", schema.Additional)
	}
}
