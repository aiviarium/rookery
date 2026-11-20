package apply_test

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
	"time"

	"golang.org/x/crypto/ssh"
)

// TestMain is also the fake ansible-playbook: a test's PATH holds a script that
// runs this binary again with ROOKERY_FAKE_ANSIBLE_RECORD set, so the fake is the
// test binary itself, re-executed.
func TestMain(m *testing.M) {
	if record := os.Getenv("ROOKERY_FAKE_ANSIBLE_RECORD"); record != "" {
		os.Exit(fakeAnsible(record))
	}
	os.Exit(m.Run())
}

// recorded is what the fake ansible-playbook saw.
type recorded struct {
	Args  []string          `json:"args"`
	Dir   string            `json:"dir"`
	Env   map[string]string `json:"env"`
	Files map[string]string `json:"files"`
	Modes map[string]string `json:"modes"`
	// Roles maps each role to what it resolves to, or to "dir" when it is one
	// that was extracted.
	Roles map[string]string `json:"roles"`
}

// The files the fake reads out of its work directory, and the ones only the test
// cares about the mode of.
var recordedFiles = []string{"inventory.yml", "site.yml", "ansible.cfg", "vars.json", "key", "known_hosts"}

// fakeAnsible runs one fake apply and records it.
func fakeAnsible(record string) int {
	seen := recorded{
		Args:  os.Args[1:],
		Env:   map[string]string{},
		Files: map[string]string{},
		Modes: map[string]string{},
		Roles: map[string]string{},
	}
	for _, line := range os.Environ() {
		name, value, ok := strings.Cut(line, "=")
		if ok {
			seen.Env[name] = value
		}
	}
	dir, err := os.Getwd()
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake ansible-playbook:", err)
		return 1
	}
	seen.Dir = dir
	for _, name := range recordedFiles {
		//nolint:gosec // the fake reads the work directory it was run in.
		data, err := os.ReadFile(filepath.Join(dir, name))
		if err != nil {
			continue
		}
		seen.Files[name] = string(data)
		info, err := os.Stat(filepath.Join(dir, name))
		if err != nil {
			continue
		}
		seen.Modes[name] = fmt.Sprintf("%04o", info.Mode().Perm())
	}
	entries, err := os.ReadDir(filepath.Join(dir, "roles"))
	if err == nil {
		names := make([]string, 0, len(entries))
		for _, entry := range entries {
			names = append(names, entry.Name())
		}
		sort.Strings(names)
		for _, name := range names {
			target, err := filepath.EvalSymlinks(filepath.Join(dir, "roles", name))
			if err != nil {
				seen.Roles[name] = "broken"
				continue
			}
			if entry, err := os.Stat(filepath.Join(dir, "roles", name)); err == nil && entry.IsDir() {
				seen.Roles[name] = target
			}
		}
	}

	encoded, err := json.Marshal(seen)
	if err != nil {
		fmt.Fprintln(os.Stderr, "fake ansible-playbook:", err)
		return 1
	}
	//nolint:gosec // the fake writes its record where the test pointed it.
	if err := os.WriteFile(filepath.Join(record, "record.json"), encoded, 0o600); err != nil {
		fmt.Fprintln(os.Stderr, "fake ansible-playbook:", err)
		return 1
	}

	if secret := os.Getenv("ROOKERY_FAKE_ANSIBLE_SECRET"); secret != "" {
		fmt.Printf("PLAY [the token is %s]\n", secret)
		fmt.Fprintf(os.Stderr, "warning: %s in a task\n", secret)
	}
	fmt.Println("PLAY RECAP: ok=1")

	switch os.Getenv("ROOKERY_FAKE_ANSIBLE_MODE") {
	case "sleep":
		time.Sleep(time.Minute)
	case "fail":
		return 2
	}
	return 0
}

// fake is a fake ansible-playbook in a directory of its own.
type fake struct {
	// Record is where the run writes what it saw.
	Record string
	// Path is the script, which can be given as the runner's playbook.
	Path string
}

// newFake writes a fake ansible-playbook that records into a directory of the
// test's making.
func newFake(t *testing.T, options ...string) *fake {
	t.Helper()

	executable, err := os.Executable()
	if err != nil {
		t.Fatalf("finding the test binary: %v", err)
	}
	dir := t.TempDir()
	record := filepath.Join(t.TempDir(), "record")
	if err := os.MkdirAll(record, 0o700); err != nil {
		t.Fatalf("making the record directory: %v", err)
	}
	script := fmt.Sprintf("#!/bin/sh\nROOKERY_FAKE_ANSIBLE_RECORD=%q", record)
	for _, option := range options {
		script += " " + option
	}
	script += fmt.Sprintf(" exec %q \"$@\"\n", executable)

	path := filepath.Join(dir, "ansible-playbook")
	//nolint:gosec // the fake is a program, and it has to be executable.
	if err := os.WriteFile(path, []byte(script), 0o755); err != nil {
		t.Fatalf("writing the fake ansible-playbook: %v", err)
	}
	return &fake{Record: record, Path: path}
}

// seen is the record of the last run.
func (f *fake) seen(t *testing.T) recorded {
	t.Helper()

	data, err := os.ReadFile(filepath.Join(f.Record, "record.json"))
	if err != nil {
		t.Fatalf("reading the record: %v", err)
	}
	var seen recorded
	if err := json.Unmarshal(data, &seen); err != nil {
		t.Fatalf("decoding the record: %v", err)
	}
	return seen
}

// maybeSeen is the record, or nothing when the run never started.
func (f *fake) maybeSeen(t *testing.T) (recorded, bool) {
	t.Helper()

	data, err := os.ReadFile(filepath.Join(f.Record, "record.json"))
	if err != nil {
		return recorded{}, false
	}
	var seen recorded
	if err := json.Unmarshal(data, &seen); err != nil {
		t.Fatalf("decoding the record: %v", err)
	}
	return seen, true
}

// testKey is a private key in the PEM form the key ring writes.
func testKey(t *testing.T) []byte {
	t.Helper()

	_, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generating a key: %v", err)
	}
	block, err := ssh.MarshalPrivateKey(private, "")
	if err != nil {
		t.Fatalf("encoding a key: %v", err)
	}
	return pem.EncodeToMemory(block)
}

// testBinary writes a file that stands in for rookeryd or box.
func testBinary(t *testing.T) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "binary")
	//nolint:gosec // the stand-in is a program, and it has to be executable.
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"), 0o755); err != nil {
		t.Fatalf("writing a binary: %v", err)
	}
	return path
}

// pluginFolder is an installed plugin with the feature's role in it.
func pluginFolder(t *testing.T, plugin, feature string) string {
	t.Helper()

	dir := t.TempDir()
	tasks := filepath.Join(dir, "features", feature, "tasks")
	//nolint:gosec // a plugin folder is a directory a role is read from.
	if err := os.MkdirAll(tasks, 0o755); err != nil {
		t.Fatalf("making a feature: %v", err)
	}
	main := filepath.Join(dir, "features", feature, "tasks", "main.yml")
	//nolint:gosec // a role's tasks are read by the fake, which reads them as the test's user.
	if err := os.WriteFile(main, []byte("---\n- name: Hello\n  ansible.builtin.debug: msg=hello\n"), 0o644); err != nil {
		t.Fatalf("writing a feature's tasks: %v", err)
	}
	_ = plugin
	return dir
}
