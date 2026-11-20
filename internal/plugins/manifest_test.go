package plugins_test

import (
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"testing/fstest"

	"github.com/aiviarium/rookery/internal/plugins"
)

// exampleManifest is the fixture package, with the files it names.
const exampleManifest = `id: example
version: 0.1.0
name: Example
requires: ">=0.1.0"
service:
  command: { linux/amd64: bin/service-amd64 }
  start: always
  rpc:
    - procedure: example.v1.NoteService/Summarize
      access: { level: view, resource: space }
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields:
      title: { type: text, required: true }
hostFeatures:
  - id: hello
    label: Hello
    types: [managed]
    tasks: features/hello/tasks/main.yml
`

// exampleContent is a package with nothing wrong with it.
func exampleContent() fstest.MapFS {
	return fstest.MapFS{
		"plugin.yaml":       {Data: []byte(exampleManifest), Mode: 0o644},
		"bin/service-amd64": {Data: []byte("#!/bin/sh\n"), Mode: 0o755},
		"features/hello/tasks/main.yml": {
			Data: []byte("---\n- name: Hello\n  ansible.builtin.debug: msg=hello\n"), Mode: 0o644,
		},
	}
}

func TestLoadTheFixture(t *testing.T) {
	t.Parallel()

	manifest, err := plugins.Load(os.DirFS(filepath.Join("..", "..", "testdata", "plugins", "example")))
	if err != nil {
		t.Fatalf("Load() = %v", err)
	}
	if manifest.ID != "example" || manifest.Version != "0.1.0" {
		t.Errorf("Load() = %s@%s", manifest.ID, manifest.Version)
	}
	if manifest.Name != "Example" {
		t.Errorf("name = %q", manifest.Name)
	}
	if len(manifest.Collections) != 1 {
		t.Fatalf("collections = %v", manifest.Collections)
	}
	notes := manifest.Collections["notes"]
	if notes.Scope != plugins.ScopeSpace || !notes.History {
		t.Errorf("notes = %+v", notes)
	}
	if notes.Fields["title"].Type != plugins.FieldText || !notes.Fields["title"].Required {
		t.Errorf("title = %+v", notes.Fields["title"])
	}
	if notes.Fields["about"].OnDelete != plugins.OnDeleteNullify {
		t.Errorf("about = %+v", notes.Fields["about"])
	}
	if entity := notes.Entity; entity == nil || entity.Kind != "Note" {
		t.Errorf("entity = %+v", entity)
	}
	if manifest.Database != plugins.DatabaseSQL {
		t.Errorf("database = %q", manifest.Database)
	}
	if len(manifest.HostFeatures) != 1 || manifest.HostFeatures[0].ID != "hello" {
		t.Errorf("hostFeatures = %+v", manifest.HostFeatures)
	}
	if got := manifest.HostFeatures[0].Actions[1]; got.User != plugins.ActionDesktop || !got.OnHost {
		t.Errorf("the desktop action = %+v", got)
	}
	if manifest.Service == nil || manifest.Service.Command["linux/amd64"] != "bin/service-amd64" || manifest.Service.Start != plugins.StartLazy {
		t.Errorf("service = %+v", manifest.Service)
	}
	if job := manifest.Jobs[0]; job.Timeout.Duration().String() != "10m0s" {
		t.Errorf("timeout = %v", job.Timeout.Duration())
	}
	if len(manifest.Provisioners) != 1 || manifest.Provisioners[0].ID != "fake" {
		t.Errorf("provisioners = %+v", manifest.Provisioners)
	}
	if contributes := manifest.Contributes; contributes == nil || len(contributes.Views) != 1 {
		t.Errorf("contributes = %+v", contributes)
	}
}

func TestCheckAcceptsAWholePackage(t *testing.T) {
	t.Parallel()

	if _, err := plugins.Check(exampleContent()); err != nil {
		t.Fatalf("Check() = %v", err)
	}
}

func TestCheckNeedsTheFilesItNames(t *testing.T) {
	t.Parallel()

	content := exampleContent()
	delete(content, "bin/service-amd64")
	if _, err := plugins.Check(content); !errors.Is(err, plugins.ErrInvalid) {
		t.Fatalf("Check() = %v, want a refusal", err)
	}

	// Load checks the manifest alone, which is what a fixture without the code
	// needs.
	if _, err := plugins.Load(content); err != nil {
		t.Errorf("Load() = %v, want the manifest alone to be enough", err)
	}
}

func TestLoadRefuses(t *testing.T) {
	t.Parallel()

	cases := []struct {
		name     string
		manifest string
	}{
		{"no id", "version: 1.0.0\nname: Example\n"},
		{"an id that is not one", "id: Example\nversion: 1.0.0\nname: Example\n"},
		{"no version", "id: example\nname: Example\n"},
		{"a partial version", "id: example\nversion: 1.0\nname: Example\n"},
		{"no name", "id: example\nversion: 1.0.0\n"},
		{"a requirement that is not one", "id: example\nversion: 1.0.0\nname: E\nrequires: soon\n"},
		{"a dependency on itself", "id: example\nversion: 1.0.0\nname: E\ndependencies: {example: '^1'}\n"},
		{"a service with both kinds", `id: example
version: 1.0.0
name: E
service:
  command: { linux/amd64: bin/x }
  runtime: bun
  entry: service/main.ts
`},
		{"a service with neither", "id: example\nversion: 1.0.0\nname: E\nservice: {start: lazy}\n"},
		{"a service with no amd64 binary", "id: example\nversion: 1.0.0\nname: E\nservice:\n  command: { linux/arm64: bin/x }\n"},
		{"a service with a path that leaves", "id: example\nversion: 1.0.0\nname: E\nservice:\n  command: { linux/amd64: ../x }\n"},
		{"an unknown service start", "id: example\nversion: 1.0.0\nname: E\nservice:\n  command: { linux/amd64: bin/x }\n  start: never\n"},
		{"a procedure that is not one", `id: example
version: 1.0.0
name: E
service:
  command: { linux/amd64: bin/x }
  rpc: [{ procedure: Summarize, access: { level: view, resource: space } }]
`},
		{"a level that is not one", `id: example
version: 1.0.0
name: E
service:
  command: { linux/amd64: bin/x }
  rpc: [{ procedure: a.B/C, access: { level: read, resource: space } }]
`},
		{"a collection without a scope", `id: example
version: 1.0.0
name: E
collections:
  notes:
    access: { read: view, write: use }
    fields: { title: { type: text } }
`},
		{"a collection without fields", "id: example\nversion: 1.0.0\nname: E\ncollections:\n  notes:\n    scope: space\n    access: { read: view, write: use }\n"},
		{"a field type that is not one", `id: example
version: 1.0.0
name: E
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields: { title: { type: html } }
`},
		{"an enum without values", `id: example
version: 1.0.0
name: E
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields: { state: { type: enum } }
`},
		{"a default that is not the field's", `id: example
version: 1.0.0
name: E
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields: { done: { type: bool, default: yes please } }
`},
		{"an index on no field", `id: example
version: 1.0.0
name: E
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields: { title: { type: text } }
    indexes: [[body]]
`},
		{"an entity whose title is no field", `id: example
version: 1.0.0
name: E
collections:
  notes:
    scope: space
    access: { read: view, write: use }
    fields: { title: { type: text } }
    entity: { kind: Note, title: name }
`},
		{"a database that is not one", "id: example\nversion: 1.0.0\nname: E\ndatabase: mongodb\n"},
		{"a schedule for a job that is not declared", "id: example\nversion: 1.0.0\nname: E\nschedules: [{kind: reindex, cron: '0 3 * * *'}]\n"},
		{"a cron that is not one", "id: example\nversion: 1.0.0\nname: E\njobs: [{kind: reindex, enqueue: {level: manage, resource: space}}]\nschedules: [{kind: reindex, cron: 'every day'}]\n"},
		{"a webhook with two verifications", `id: example
version: 1.0.0
name: E
secrets: [token]
jobs: [{kind: reindex, enqueue: {level: manage, resource: space}}]
webhooks:
  - name: push
    job: reindex
    verify:
      hmac-sha256: { header: X-Signature, secret: token }
      token: { header: X-Token, secret: token }
`},
		{"a webhook with an undeclared secret", `id: example
version: 1.0.0
name: E
jobs: [{kind: reindex, enqueue: {level: manage, resource: space}}]
webhooks:
  - { name: push, job: reindex, verify: { hmac-sha256: { header: X-Signature, secret: nope } } }
`},
		{"an event of another plugin", "id: example\nversion: 1.0.0\nname: E\nevents: [other/note.changed]\n"},
		{"a feature that applies to nothing", "id: example\nversion: 1.0.0\nname: E\nhostFeatures: [{id: hello, types: [], tasks: features/hello/tasks/main.yml}]\n"},
		{"a feature with an action that runs as nobody", `id: example
version: 1.0.0
name: E
hostFeatures:
  - id: hello
    types: [managed]
    tasks: features/hello/tasks/main.yml
    actions: [{name: say, user: everyone, run: echo}]
`},
		{"a shell without a client half", "id: example\nversion: 1.0.0\nname: E\nshell: true\n"},
		{"a contribution without a client half", "id: example\nversion: 1.0.0\nname: E\ncontributes: {views: []}\n"},
		{"a navigation entry for a view that is not declared", `id: example
version: 1.0.0
name: E
client: web/
contributes: {nav: [{view: notes}]}
`},
		{"a key Rookery does not know", "id: example\nversion: 1.0.0\nname: E\ncolour: blue\n"},
	}

	for _, c := range cases {
		t.Run(c.name, func(t *testing.T) {
			t.Parallel()
			content := fstest.MapFS{"plugin.yaml": {Data: []byte(c.manifest), Mode: 0o644}}
			_, err := plugins.Load(content)
			if !errors.Is(err, plugins.ErrInvalid) {
				t.Fatalf("Load() = %v, want a refusal", err)
			}
		})
	}
}

func TestLoadWithoutAManifest(t *testing.T) {
	t.Parallel()

	if _, err := plugins.Load(fstest.MapFS{}); !errors.Is(err, plugins.ErrInvalid) {
		t.Fatalf("Load() = %v, want a refusal", err)
	}
	if _, err := plugins.Load(fstest.MapFS{"plugin.yaml": {Data: []byte("")}}); !errors.Is(err, plugins.ErrInvalid) {
		t.Fatalf("Load(empty) = %v, want a refusal", err)
	}
}

func TestLoadRefusesTwoThingsAtOnce(t *testing.T) {
	t.Parallel()

	// The first mistake is the one reported, and it names the manifest.
	content := fstest.MapFS{"plugin.yaml": {Data: []byte("id: Example\nversion: nope\nname: E\n")}}
	_, err := plugins.Load(content)
	if err == nil {
		t.Fatal("Load() accepted a broken manifest")
	}
	if !strings.Contains(err.Error(), "is not a plugin id") {
		t.Errorf("error = %v, want the id first", err)
	}
}
