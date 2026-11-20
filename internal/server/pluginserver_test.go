package server

import (
	"context"
	"testing"
	"time"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/messaging"
)

// TestAPluginPublishesUnderItsOwnName checks the namespace and the principal
// that ServerService.Publish requires.
func TestAPluginPublishesUnderItsOwnName(t *testing.T) {
	bus := messaging.NewBus()
	events := bus.Subscribe("", nil)
	defer events.Close()

	service := NewPluginServer(bus, nil)
	ctx := access.WithPrincipal(context.Background(), access.Plugin("example"))

	if _, err := service.Publish(ctx, connect.NewRequest(&pluginv1.ServerServicePublishRequest{Name: "other/note.changed"})); err == nil {
		t.Fatal("a plugin published an event outside its own id")
	}

	// A user's token is not a plugin's.
	userCtx := access.WithPrincipal(context.Background(), access.Principal{Kind: access.KindUser, ID: "01h"})
	if _, err := service.Publish(userCtx, connect.NewRequest(&pluginv1.ServerServicePublishRequest{Name: "example/note.changed"})); err == nil {
		t.Fatal("a user published a plugin's event")
	}

	if _, err := service.Publish(ctx, connect.NewRequest(&pluginv1.ServerServicePublishRequest{
		Name:        "example/note.changed",
		Resource:    access.PluginURI("example"),
		PayloadCbor: []byte{0xa0},
	})); err != nil {
		t.Fatalf("publishing: %v", err)
	}

	select {
	case event := <-events.Events:
		if event.Name != "example/note.changed" {
			t.Fatalf("the event is %q", event.Name)
		}
	case <-time.After(time.Second):
		t.Fatal("the event was not published")
	}
}
