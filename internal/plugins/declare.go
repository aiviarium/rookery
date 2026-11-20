package plugins

import (
	"fmt"
	"strings"
	"time"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/data"
	"github.com/aiviarium/rookery/internal/filter"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/spaces"
)

// The declarations a plugin makes, as the packages that own them read them: its
// collections are the datastore's, its jobs are the runner's, and its kinds of
// space are the spaces'.
//
// The manifest is the plugin's to write, so every conversion here refuses what
// it cannot express rather than guessing.

// collectionOf is a manifest collection as the datastore declares it.
func collectionOf(plugin, name string, declared Collection) (*data.Collection, error) {
	collection := &data.Collection{
		Plugin:  plugin,
		Name:    name,
		Scope:   data.Scope(declared.Scope),
		Access:  data.Access{Read: levelOf(declared.Access.Read), Write: levelOf(declared.Access.Write)},
		History: declared.History,
		Fields:  make(map[string]data.Field, len(declared.Fields)),
		Indexes: declared.Indexes,
	}
	for fieldName, declaredField := range declared.Fields {
		field, err := fieldOf(declaredField)
		if err != nil {
			return nil, fmt.Errorf("plugins: the field %s.%s of %s: %w", name, fieldName, plugin, err)
		}
		collection.Fields[fieldName] = field
	}
	if declared.Entity != nil {
		collection.Entity = &data.Entity{
			Kind:  declared.Entity.Kind,
			Title: declared.Entity.Title,
			Text:  declared.Entity.Text,
		}
	}
	return collection, nil
}

// fieldOf is a manifest field as the datastore declares it.
func fieldOf(declared Field) (data.Field, error) {
	kind, err := filter.ParseType(string(declared.Type))
	if err != nil {
		return data.Field{}, err
	}
	size := 0
	if kind == filter.TypeVector {
		size = vectorSize(string(declared.Type))
	}
	return data.Field{
		Type:     kind,
		Size:     size,
		Values:   declared.Enum,
		Required: declared.Required,
		Default:  declared.Default,
		OnDelete: data.OnDelete(declared.OnDelete),
	}, nil
}

// vectorSize is the dimension a "vector(N)" field declares.
func vectorSize(declaration string) int {
	_, size, found := strings.Cut(declaration, "(")
	if !found {
		return 0
	}
	size = strings.TrimSuffix(size, ")")
	dimensions := 0
	for _, digit := range size {
		if digit < '0' || digit > '9' {
			return 0
		}
		dimensions = dimensions*10 + int(digit-'0')
	}
	return dimensions
}

// levelOf is a manifest level as the edge API has it.
func levelOf(level Level) rookeryv1.Level {
	switch level {
	case LevelManage:
		return rookeryv1.Level_LEVEL_MANAGE
	case LevelUse:
		return rookeryv1.Level_LEVEL_USE
	default:
		return rookeryv1.Level_LEVEL_VIEW
	}
}

// kindOf is a manifest job as the runner's registry declares it.
func kindOf(plugin string, declared Job) (jobs.Kind, error) {
	if declared.Kind == "" {
		return jobs.Kind{}, fmt.Errorf("%w: %s declares a job with no kind", access.ErrInvalid, plugin)
	}
	level := access.Level(levelOf(declared.Enqueue.Level))
	if declared.Enqueue.Level == "" {
		level = access.Use
	}
	return jobs.Kind{
		Name:         plugin + "/" + declared.Kind,
		Level:        level,
		ResourceKind: string(declared.Enqueue.Resource),
		Timeout:      time.Duration(declared.Timeout),
		Retries:      declared.Retries,
	}, nil
}

// spaceKindOf is a manifest kind of space as the spaces' registry declares it.
func spaceKindOf(plugin string, declared SpaceKind) spaces.Kind {
	return spaces.Kind{ID: declared.ID, Label: declared.Label, Plugin: plugin}
}
