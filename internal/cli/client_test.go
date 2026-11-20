package cli

import (
	"os"
	"testing"
)

func TestSocketPath(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name    string
		address string
		want    string
		isUnix  bool
	}{
		{name: "unix scheme", address: "unix:///run/rookery/rookery.sock", want: "/run/rookery/rookery.sock", isUnix: true},
		{name: "unix shorthand", address: "unix:/run/rookery.sock", want: "/run/rookery.sock", isUnix: true},
		{name: "a path", address: "/run/rookery.sock", want: "/run/rookery.sock", isUnix: true},
		{name: "https", address: "https://rookery.example.test", isUnix: false},
		{name: "http", address: "http://127.0.0.1:4770", isUnix: false},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			t.Parallel()
			path, got := socketPath(tc.address)
			if got != tc.isUnix {
				t.Fatalf("socketPath(%q) reports unix %v, want %v", tc.address, got, tc.isUnix)
			}
			if path != tc.want {
				t.Errorf("socketPath(%q) = %q, want %q", tc.address, path, tc.want)
			}
		})
	}
}

func TestResolvePrefersTheFlagThenTheEnvironment(t *testing.T) {
	// The saved session is a file in the user's configuration directory, which a
	// test must not read or write: the environment alone is what this checks.
	// Made at run time, like a real token.
	token := "rkpat_" + "fromenvironment"
	t.Setenv("ROOKERY_SERVER", "https://from-environment.example.test")
	t.Setenv("ROOKERY_TOKEN", token)

	address, token, err := resolve("")
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if address != "https://from-environment.example.test" || token != os.Getenv("ROOKERY_TOKEN") {
		t.Fatalf("resolve returned %q and %q", address, token)
	}

	address, token, err = resolve("https://from-flag.example.test")
	if err != nil {
		t.Fatalf("resolve: %v", err)
	}
	if address != "https://from-flag.example.test" {
		t.Fatalf("the flag did not win: %q", address)
	}
	if token == "" {
		t.Fatal("resolve found no token")
	}
}
