package provisioner

import (
	"reflect"
	"strings"
	"testing"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// TestParseArguments checks an entry's arguments and their defaults.
func TestParseArguments(t *testing.T) {
	t.Parallel()

	tests := map[string]struct {
		encoded []byte
		want    arguments
		fails   bool
	}{
		"none": {
			encoded: nil,
			want: arguments{
				Image:   DefaultMachineImage,
				Size:    "small",
				Network: DefaultNetwork,
			},
		},
		"a machine on a network of its own": {
			encoded: mustMarshal(t, map[string]any{
				"image":   "rookery/machine:dev",
				"size":    "large",
				"runtime": "sysbox-runc",
				"network": "lab",
				"socket":  "/run/docker.sock",
			}),
			want: arguments{
				Image:   "rookery/machine:dev",
				Size:    "large",
				Runtime: "sysbox-runc",
				Network: "lab",
				Socket:  "/run/docker.sock",
			},
		},
		"a compose file": {
			encoded: mustMarshal(t, map[string]any{
				"file":        "/srv/app/compose.yaml",
				"project":     "app",
				"environment": map[string]string{"TAG": "1.2"},
			}),
			want: arguments{
				Image:       DefaultMachineImage,
				Size:        "small",
				Network:     DefaultNetwork,
				File:        "/srv/app/compose.yaml",
				Project:     "app",
				Environment: map[string]string{"TAG": "1.2"},
			},
		},
		"not an object": {
			encoded: mustMarshal(t, []string{"no"}),
			fails:   true,
		},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			t.Parallel()

			parsed, err := parseArguments(test.encoded)
			if test.fails {
				if err == nil {
					t.Fatalf("%+v parsed", test.encoded)
				}
				return
			}
			if err != nil {
				t.Fatalf("parsing: %v", err)
			}
			if !reflect.DeepEqual(parsed, test.want) {
				t.Fatalf("the arguments are %+v, want %+v", parsed, test.want)
			}
		})
	}
}

// TestSizes checks the three sizes and what Docker is given for them.
func TestSizes(t *testing.T) {
	t.Parallel()

	for name, want := range map[string]size{
		"small":  {cpus: 2, memoryMB: 2048, diskGB: 20},
		"medium": {cpus: 4, memoryMB: 8192, diskGB: 50},
		"large":  {cpus: 8, memoryMB: 16384, diskGB: 100},
	} {
		arguments := arguments{Size: name}
		parsed, err := arguments.size()
		if err != nil {
			t.Fatalf("%s: %v", name, err)
		}
		if parsed != want {
			t.Errorf("%s is %+v, want %+v", name, parsed, want)
		}
		if parsed.nanoCPUs() != int64(want.cpus*1e9) {
			t.Errorf("%s is %d nanocpus", name, parsed.nanoCPUs())
		}
		if parsed.memory() <= 0 {
			t.Errorf("%s has no memory limit", name)
		}
	}

	if _, err := (arguments{Size: "huge"}).size(); err == nil {
		t.Fatal("a size that is not one worked")
	}
}

// TestDockerName checks the names Docker accepts, which an entry's own name is
// not: it holds the type, and a type holds a slash.
func TestDockerName(t *testing.T) {
	t.Parallel()

	tests := map[string]string{
		"lab-docker/container":  "lab-docker-container",
		"Docker":                "docker",
		"a b":                   "a-b",
		"-lead-":                "lead",
		strings.Repeat("a", 80): strings.Repeat("a", 63),
	}
	for name, want := range tests {
		if got := dockerName(name); got != want {
			t.Errorf("dockerName(%q) = %q, want %q", name, got, want)
		}
	}
	if !validName("desk-1") {
		t.Error("desk-1 is not a valid name")
	}
	if validName("desk/1") {
		t.Error("desk/1 is a valid name")
	}
}

// mustMarshal encodes a value, failing the test if it cannot.
func mustMarshal(t *testing.T, value any) []byte {
	t.Helper()

	encoded, err := sdk.Marshal(value)
	if err != nil {
		t.Fatalf("encoding: %v", err)
	}
	return encoded
}
