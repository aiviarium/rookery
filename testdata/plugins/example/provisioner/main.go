// The example plugin's fake provisioner.
//
// It makes nothing: a machine is a directory with its cloud-init seed in it, and
// the address it reports is the one its arguments name. It is what the core's
// tests use to start a real provisioner the way the server does, and to see what
// the core asked it for.
package main

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"

	"github.com/aiviarium/rookery/pkg/sdk"
)

func main() {
	if err := sdk.Run(fake{}); err != nil {
		fmt.Fprintf(os.Stderr, "fake provisioner: %v\n", err)
		os.Exit(1)
	}
}

// arguments are the entry's arguments: where machines are kept, and the address
// they report.
type arguments struct {
	Dir     string `cbor:"dir"`
	Address string `cbor:"address"`
}

// state is what the provisioner needs to reach a machine again.
type state struct {
	Dir     string `cbor:"dir"`
	Address string `cbor:"address"`
}

// fake is a provisioner that keeps machines in a directory.
type fake struct{}

func (fake) Describe(context.Context) (sdk.Description, error) {
	return sdk.Description{
		Type:  "example/fake",
		Label: "Fake machine",
		Kinds: []sdk.Kind{{ID: "machine", Label: "Machine", Machine: true}},
		Arguments: []sdk.Argument{
			{Name: "dir", Label: "Directory", Type: "string", Required: true, Help: "where machines are kept"},
			{Name: "address", Label: "Address", Type: "string", Help: "the address every machine reports"},
		},
		Secrets: []string{"example_token"},
		Sizes:   []sdk.Size{{ID: "small", Label: "Small", CPUs: 2, MemoryMB: 2048, DiskGB: 20}},
		Actions: []sdk.Action{
			{Name: "machines", Label: "Machines", Group: "Fake"},
			{Name: "seed", Label: "Seed", Group: "Fake", Arguments: []sdk.Argument{{Name: "name", Type: "string", Required: true}}},
			{Name: "fail", Label: "Fail", Group: "Fake", Destructive: true},
		},
		Node:  true,
		Local: true,
	}, nil
}

func (fake) Status(_ context.Context, engine sdk.Engine, args []byte) (sdk.Status, error) {
	parsed, err := parse(args)
	if err != nil {
		return sdk.Status{}, err
	}
	if _, err := os.Stat(parsed.Dir); err != nil {
		return sdk.Status{Up: false, Message: "no directory " + parsed.Dir}, nil
	}
	secrets, err := sdk.Secrets()
	if err != nil {
		return sdk.Status{}, err
	}
	message := "fake"
	if _, ok := secrets["example_token"]; ok {
		message += ", token set"
	}
	if config, err := sdk.ConfigFromEnv(); err == nil && config.Socket != "" {
		message += ", socket set"
	}
	switch {
	case engine.Node != "":
		message += ", node " + engine.Node
	case engine.Remote != "":
		message += ", remote " + engine.Remote
	default:
		message += ", local"
	}
	return sdk.Status{Up: true, Message: message}, nil
}

func (fake) Create(_ context.Context, request sdk.CreateRequest) (sdk.Instance, error) {
	parsed, err := parse(request.Args)
	if err != nil {
		return sdk.Instance{}, err
	}
	if request.Kind != "machine" {
		return sdk.Instance{}, fmt.Errorf("the fake makes machines, not %s", request.Kind)
	}
	if request.Name == "" {
		return sdk.Instance{}, errors.New("a machine needs a name")
	}
	if request.Seed == "" {
		return sdk.Instance{}, errors.New("a machine needs a seed")
	}
	dir := filepath.Join(parsed.Dir, request.Name)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return sdk.Instance{}, fmt.Errorf("making %s: %w", dir, err)
	}
	if err := os.WriteFile(filepath.Join(dir, "user-data"), []byte(request.Seed), 0o600); err != nil {
		return sdk.Instance{}, fmt.Errorf("writing the seed: %w", err)
	}
	if err := os.WriteFile(filepath.Join(dir, "running"), nil, 0o600); err != nil {
		return sdk.Instance{}, fmt.Errorf("writing the state: %w", err)
	}
	encoded, err := sdk.Marshal(state{Dir: parsed.Dir, Address: parsed.Address})
	if err != nil {
		return sdk.Instance{}, err
	}
	return sdk.Instance{Kind: "machine", Name: request.Name, ID: request.Name, State: encoded, Address: parsed.Address}, nil
}

func (fake) Get(_ context.Context, instance sdk.Instance) (sdk.Instance, error) {
	parsed, err := parseState(instance.State)
	if err != nil {
		return sdk.Instance{}, err
	}
	if _, err := os.Stat(filepath.Join(parsed.Dir, instance.Name)); err != nil {
		return sdk.Instance{}, fmt.Errorf("no machine %s: %w", instance.Name, err)
	}
	instance.Address = parsed.Address
	return instance, nil
}

func (fake) Start(ctx context.Context, instance sdk.Instance) (sdk.Instance, error) {
	return fake{}.mark(ctx, instance, "running")
}

func (fake) Stop(ctx context.Context, instance sdk.Instance) error {
	_, err := fake{}.mark(ctx, instance, "stopped")
	return err
}

// mark writes the machine's state, which the fake's List reads.
func (fake) mark(_ context.Context, instance sdk.Instance, name string) (sdk.Instance, error) {
	parsed, err := parseState(instance.State)
	if err != nil {
		return sdk.Instance{}, err
	}
	dir := filepath.Join(parsed.Dir, instance.Name)
	if _, err := os.Stat(dir); err != nil {
		return sdk.Instance{}, fmt.Errorf("no machine %s: %w", instance.Name, err)
	}
	if err := os.WriteFile(filepath.Join(dir, name), nil, 0o600); err != nil {
		return sdk.Instance{}, fmt.Errorf("writing the state: %w", err)
	}
	instance.Address = parsed.Address
	return instance, nil
}

func (fake) Remove(_ context.Context, instance sdk.Instance) error {
	parsed, err := parseState(instance.State)
	if err != nil {
		return err
	}
	return os.RemoveAll(filepath.Join(parsed.Dir, instance.Name))
}

func (fake) List(_ context.Context, request sdk.ListRequest) ([]sdk.Resource, error) {
	parsed, err := parse(request.Args)
	if err != nil {
		return nil, err
	}
	entries, err := os.ReadDir(parsed.Dir)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading %s: %w", parsed.Dir, err)
	}
	resources := make([]sdk.Resource, 0, len(entries))
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		encoded, err := sdk.Marshal(state{Dir: parsed.Dir, Address: parsed.Address})
		if err != nil {
			return nil, err
		}
		resources = append(resources, sdk.Resource{
			Kind:    "machine",
			Name:    entry.Name(),
			ID:      entry.Name(),
			Address: parsed.Address,
			State:   encoded,
			Labels: map[string]string{
				"user.rookery.managed": "true",
				"user.rookery.entry":   request.Entry,
				"user.rookery.kind":    request.Kind,
				"user.rookery.name":    entry.Name(),
			},
		})
	}
	sort.Slice(resources, func(i, j int) bool { return resources[i].Name < resources[j].Name })
	return resources, nil
}

func (f fake) RunAction(_ context.Context, request sdk.ActionRequest, line func(string)) error {
	switch request.Action {
	case "machines":
		parsed, err := parse(request.EntryArgs)
		if err != nil {
			return err
		}
		entries, err := os.ReadDir(parsed.Dir)
		if errors.Is(err, os.ErrNotExist) {
			return nil
		}
		if err != nil {
			return fmt.Errorf("reading %s: %w", parsed.Dir, err)
		}
		for _, entry := range entries {
			line(entry.Name())
		}
		return nil
	case "seed":
		var args struct {
			Name string `cbor:"name"`
		}
		if err := sdk.Unmarshal(request.Args, &args); err != nil {
			return fmt.Errorf("the arguments are not {name}: %w", err)
		}
		parsed, err := parse(request.EntryArgs)
		if err != nil {
			return err
		}
		seed, err := os.ReadFile(filepath.Join(parsed.Dir, args.Name, "user-data"))
		if err != nil {
			return fmt.Errorf("no seed for %s: %w", args.Name, err)
		}
		for _, text := range splitLines(string(seed)) {
			line(text)
		}
		return nil
	case "fail":
		return errors.New("the fake failed on purpose")
	default:
		return fmt.Errorf("the fake has no action %q", request.Action)
	}
}

// parse reads an entry's arguments.
func parse(args []byte) (arguments, error) {
	parsed := arguments{Address: "192.0.2.10"}
	if len(args) > 0 {
		if err := sdk.Unmarshal(args, &parsed); err != nil {
			return arguments{}, fmt.Errorf("the arguments are not {dir, address}: %w", err)
		}
	}
	if parsed.Dir == "" {
		return arguments{}, errors.New("the arguments name no directory")
	}
	return parsed, nil
}

// parseState reads what the provisioner stored about a machine.
func parseState(encoded []byte) (state, error) {
	var parsed state
	if err := sdk.Unmarshal(encoded, &parsed); err != nil {
		return state{}, fmt.Errorf("the state is not {dir, address}: %w", err)
	}
	if parsed.Dir == "" {
		return state{}, errors.New("the state names no directory")
	}
	return parsed, nil
}

// splitLines is a string's lines, without the empty one after a final newline.
func splitLines(text string) []string {
	lines := []string{}
	start := 0
	for i := 0; i < len(text); i++ {
		if text[i] == '\n' {
			lines = append(lines, text[start:i])
			start = i + 1
		}
	}
	if start < len(text) {
		lines = append(lines, text[start:])
	}
	return lines
}
