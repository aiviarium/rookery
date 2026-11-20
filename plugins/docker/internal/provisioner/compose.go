package provisioner

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/compose-spec/compose-go/v2/loader"
	"github.com/compose-spec/compose-go/v2/types"
)

// loadProject reads a deployment's compose file: the YAML in the entry's
// arguments, or a file on the server's machine that the provisioner can read.
func loadProject(ctx context.Context, arguments arguments, name string) (*types.Project, error) {
	var details types.ConfigDetails
	switch {
	case strings.TrimSpace(arguments.Content) != "":
		details = types.ConfigDetails{
			WorkingDir: workingDir(arguments),
			ConfigFiles: []types.ConfigFile{{
				Filename: "compose.yaml",
				Content:  []byte(arguments.Content),
			}},
		}
	case strings.TrimSpace(arguments.File) != "":
		content, err := os.ReadFile(arguments.File) //nolint:gosec // the entry names the file, and an admin wrote the entry.
		if err != nil {
			return nil, fmt.Errorf("reading the compose file %s: %w", arguments.File, err)
		}
		details = types.ConfigDetails{
			WorkingDir: filepath.Dir(arguments.File),
			ConfigFiles: []types.ConfigFile{{
				Filename: filepath.Base(arguments.File),
				Content:  content,
			}},
		}
	default:
		return nil, fmt.Errorf("a deployment needs a compose file: the file or content argument")
	}
	if len(arguments.Environment) > 0 {
		details.Environment = types.Mapping(arguments.Environment)
	}

	project, err := loader.LoadWithContext(ctx, details, func(options *loader.Options) {
		options.SetProjectName(name, true)
		options.ResolvePaths = true
	})
	if err != nil {
		return nil, fmt.Errorf("reading the compose file: %w", err)
	}
	return project, nil
}

// workingDir is where a compose file's relative paths start, when the YAML came
// in the arguments: the server's working directory.
func workingDir(arguments arguments) string {
	if arguments.File != "" {
		return filepath.Dir(arguments.File)
	}
	dir, err := os.Getwd()
	if err != nil {
		return "."
	}
	return dir
}

// serviceOrder is the services to start, each after what it depends on.
func serviceOrder(project *types.Project) ([]string, error) {
	names := project.ServiceNames()
	order := make([]string, 0, len(names))
	done := map[string]bool{}
	var visit func(name string, path []string) error
	visit = func(name string, path []string) error {
		if done[name] {
			return nil
		}
		for _, seen := range path {
			if seen == name {
				return fmt.Errorf("the compose file has a cycle: %s", strings.Join(append(path, name), " -> "))
			}
		}
		service, ok := project.Services[name]
		if !ok {
			return fmt.Errorf("the compose file names a service %s it does not have", name)
		}
		for dependency := range service.DependsOn {
			if err := visit(dependency, append(path, name)); err != nil {
				return err
			}
		}
		done[name] = true
		order = append(order, name)
		return nil
	}
	for _, name := range names {
		if err := visit(name, nil); err != nil {
			return nil, err
		}
	}
	return order, nil
}
