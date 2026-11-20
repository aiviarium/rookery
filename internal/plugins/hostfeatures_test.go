package plugins_test

import (
	"bytes"
	"os"
	"path/filepath"
	"runtime"
	"slices"
	"testing"

	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/plugins"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/provision"
	"github.com/aiviarium/rookery/internal/store/storetest"
)

func TestBundledFeatureOffers(t *testing.T) {
	t.Parallel()
	if runtime.GOOS != "linux" || (runtime.GOARCH != "amd64" && runtime.GOARCH != "arm64") {
		t.Skip("only Linux has the first-party provisioners")
	}
	ctx := t.Context()
	opened := storetest.New(t)
	options := plugins.Options{Store: opened, StateDir: t.TempDir(), Version: "0.1.0"}
	installer := plugins.NewInstaller(options)

	for _, id := range []string{"docker", "incus"} {
		folder := t.TempDir()
		source := os.DirFS(filepath.Join("..", "..", "plugins", id))
		if err := os.CopyFS(folder, source); err != nil {
			t.Fatal(err)
		}
		manifest, err := plugins.Load(source)
		if err != nil {
			t.Fatal(err)
		}
		if err := os.Mkdir(filepath.Join(folder, "bin"), 0o750); err != nil {
			t.Fatal(err)
		}
		for _, typ := range manifest.Provisioners {
			for _, path := range typ.Command {
				// The catalog test needs a package path. Module tests exercise the engines.
				// The command must be executable for the catalog to find it.
				if err := os.WriteFile(filepath.Join(folder, path), []byte("#!/bin/sh\nexit 1\n"), 0o700); err != nil { //nolint:gosec // a command the provisioner runs
					t.Fatal(err)
				}
			}
		}
		var data bytes.Buffer
		if _, err := plugins.Pack(ctx, &data, folder); err != nil {
			t.Fatal(err)
		}
		if _, err := installer.Install(ctx, &data); err != nil {
			t.Fatal(err)
		}
	}

	restored := plugins.NewInstaller(plugins.Options{Store: opened, StateDir: t.TempDir()})
	if err := restored.Restore(ctx); err != nil {
		t.Fatal(err)
	}
	features, err := restored.HostFeatures(ctx, profile.Profile{Type: profile.TypeManaged, Features: []profile.Feature{
		{Plugin: "docker", Feature: "engine"}, {Plugin: "incus", Feature: "server"},
	}}, nil)
	if err != nil {
		t.Fatal(err)
	}
	want := []string{"docker/container", "incus/vm", "incus/container"}
	if !slices.Equal(features.Offers, want) {
		t.Fatalf("offers = %v, want %v", features.Offers, want)
	}
	providers := provision.New(provision.Deps{Store: opened, Checker: access.NewChecker(opened), Catalog: restored})
	entries, err := providers.Offer(ctx, "forge", features.Offers)
	if err != nil || len(entries) != len(want) {
		t.Fatalf("node offers: %v, %v", entries, err)
	}
	for _, feature := range features.Features {
		if _, err := os.Stat(filepath.Join(feature.Dir, feature.Path)); err != nil {
			t.Fatalf("restored feature: %v", err)
		}
	}
	for _, typ := range want {
		if _, ok, err := restored.ProvisionerType(ctx, typ); err != nil || !ok {
			t.Fatalf("type %s: %v, %v", typ, ok, err)
		}
	}
}
