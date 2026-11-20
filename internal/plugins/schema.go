package plugins

import (
	"encoding/json"
	"fmt"
	"reflect"
	"strings"
)

// schemaURL is the JSON Schema dialect the manifest's schema is written in.
const schemaURL = "https://json-schema.org/draft/2020-12/schema"

// Schema is the JSON Schema of a manifest, generated from the Go types above, so
// that an editor completes and checks a plugin.yaml while it is written.
//
// It is generated rather than written by hand: `make gen` writes it to
// schemas/plugin.schema.json, and a test fails when the checked-in file and the
// types disagree.
func Schema() ([]byte, error) {
	schema := schemaFor(reflect.TypeFor[Manifest]())
	schema.Schema = schemaURL
	schema.ID = "https://github.com/aiviarium/rookery/schemas/plugin.schema.json"
	schema.Title = "plugin.yaml"
	schema.Description = "A plugin's manifest: what it is, what it adds to Rookery, and how to run it."

	encoded, err := json.MarshalIndent(schema, "", "  ")
	if err != nil {
		return nil, fmt.Errorf("plugins: writing the manifest's schema: %w", err)
	}
	return append(encoded, '\n'), nil
}

// schemaObject is the part of JSON Schema this generator writes. Everything it
// leaves out is left out of the file too, so a type with no fixed shape is an
// empty schema, which allows anything.
type schemaObject struct {
	Schema      string                  `json:"$schema,omitempty"`
	ID          string                  `json:"$id,omitempty"`
	Title       string                  `json:"title,omitempty"`
	Description string                  `json:"description,omitempty"`
	Type        string                  `json:"type,omitempty"`
	Enum        []string                `json:"enum,omitempty"`
	Pattern     string                  `json:"pattern,omitempty"`
	Properties  map[string]schemaObject `json:"properties,omitempty"`
	Required    []string                `json:"required,omitempty"`
	Items       *schemaObject           `json:"items,omitempty"`
	// Additional is false for a structure that refuses keys it does not know,
	// and a schema for a map's values.
	Additional any `json:"additionalProperties,omitempty"`
}

// enums are the schemas of the string types with a fixed set of values or a
// shape.
var enums = map[reflect.Type]schemaObject{
	reflect.TypeFor[StartMode]():  {Enum: []string{string(StartLazy), string(StartAlways)}},
	reflect.TypeFor[Level]():      {Enum: []string{string(LevelView), string(LevelUse), string(LevelManage)}},
	reflect.TypeFor[Scope]():      {Enum: []string{string(ScopeGlobal), string(ScopeSpace), string(ScopeUser)}},
	reflect.TypeFor[OnDelete]():   {Enum: []string{string(OnDeleteRestrict), string(OnDeleteNullify), string(OnDeleteCascade)}},
	reflect.TypeFor[Database]():   {Enum: []string{string(DatabaseSQL)}},
	reflect.TypeFor[ActionUser](): {Enum: []string{string(ActionOperator), string(ActionRoot), string(ActionDesktop)}},
	reflect.TypeFor[HostType]():   {Enum: []string{string(HostSSH), string(HostManaged), string(HostWorkstation)}},
	reflect.TypeFor[VariableType](): {Enum: []string{
		string(VariableString), string(VariableInt), string(VariableFloat),
		string(VariableBool), string(VariableTime), string(VariableCBOR),
	}},
	reflect.TypeFor[Resource](): {Enum: []string{
		string(ResourceHost), string(ResourceSpace), string(ResourcePlugin),
		string(ResourceRecord), string(ResourceDocument), string(ResourceProvisioner),
		string(ResourceRollout),
	}},
	// A vector's dimension is part of its type's name.
	reflect.TypeFor[FieldType](): {
		Pattern: `^(string|text|int|float|bool|time|enum|ref|cbor|vector\([0-9]+\))$`,
	},
}

// descriptions say what a part of the schema is, for the manifest's readers.
var descriptions = map[string]string{
	"service":           "The plugin's server-side code: a binary per platform, or a runtime and an entry.",
	"collections":       "The datastore collections the plugin declares, by name.",
	"settings":          "The settings the plugin declares, by name.",
	"Contributes":       "What the plugin's client half adds to the shell.",
	"Action.run":        "The command, run with its arguments as ROOKERY_ARG_* variables and never through a shell string.",
	"HostFeature.tasks": "The feature's Ansible tasks in the package; an apply links them in as the role <plugin>__<feature>.",
}

// schemaFor is the schema of a Go type.
func schemaFor(t reflect.Type) schemaObject {
	if t == reflect.TypeFor[Duration]() {
		return schemaObject{Type: "string", Pattern: `^-?([0-9]+(\.[0-9]+)?(ns|us|µs|ms|s|m|h))+$`}
	}
	if t.Kind() == reflect.Pointer {
		return schemaFor(t.Elem())
	}
	if fixed, ok := enums[t]; ok {
		fixed.Type = "string"
		return fixed
	}

	switch t.Kind() {
	case reflect.Interface:
		// A dynamic value, such as a setting's or a field's default.
		return schemaObject{}
	case reflect.String:
		return schemaObject{Type: "string"}
	case reflect.Bool:
		return schemaObject{Type: "boolean"}
	case reflect.Int, reflect.Int8, reflect.Int16, reflect.Int32, reflect.Int64,
		reflect.Uint, reflect.Uint8, reflect.Uint16, reflect.Uint32, reflect.Uint64:
		return schemaObject{Type: "integer"}
	case reflect.Float32, reflect.Float64:
		return schemaObject{Type: "number"}
	case reflect.Slice, reflect.Array:
		items := schemaFor(t.Elem())
		return schemaObject{Type: "array", Items: &items}
	case reflect.Map:
		value := schemaFor(t.Elem())
		return schemaObject{Type: "object", Additional: value}
	case reflect.Struct:
		return structSchema(t)
	}
	return schemaObject{}
}

// structSchema is the schema of a structure: its properties, the ones it needs,
// and no others, which is how the manifest is read.
func structSchema(t reflect.Type) schemaObject {
	schema := schemaObject{
		Type:       "object",
		Properties: make(map[string]schemaObject, t.NumField()),
		Additional: false,
	}
	for i := 0; i < t.NumField(); i++ {
		field := t.Field(i)
		if !field.IsExported() {
			continue
		}
		name, ok := jsonName(field)
		if !ok {
			continue
		}
		property := schemaFor(field.Type)
		if description, ok := descriptions[t.Name()+"."+name]; ok && property.Description == "" {
			property.Description = description
		} else if description, ok := descriptions[name]; ok && property.Description == "" &&
			t.Name() == "Manifest" {
			property.Description = description
		}
		schema.Properties[name] = property
		if strings.Contains(field.Tag.Get("json"), ",omitempty") || field.Type.Kind() == reflect.Pointer {
			continue
		}
		schema.Required = append(schema.Required, name)
	}
	if len(schema.Properties) == 0 {
		schema.Properties = nil
	}
	return schema
}

// jsonName is a field's name in JSON and in a manifest.
func jsonName(field reflect.StructField) (string, bool) {
	tag := field.Tag.Get("json")
	if tag == "" {
		return "", false
	}
	name, _, _ := strings.Cut(tag, ",")
	if name == "-" || name == "" {
		return "", false
	}
	return name, true
}
