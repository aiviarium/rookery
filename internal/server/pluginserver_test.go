package server

import (
	"bytes"
	"context"
	"testing"
	"time"

	"connectrpc.com/connect"

	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/store/storetest"
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

func TestPluginKVComparesRevision(t *testing.T) {
	t.Parallel()
	ctx := access.WithPrincipal(t.Context(), access.Plugin("example"))
	server := &PluginServer{store: storetest.New(t)}
	key := "counter"
	zero, first := int64(0), int64(1)
	write := func(expected *int64, value byte) (*pluginv1.ServerServiceSetKVResponse, error) {
		out, err := server.SetKV(ctx, connect.NewRequest(&pluginv1.ServerServiceSetKVRequest{
			Key: key, ValueCbor: []byte{value}, ExpectedRevision: expected,
		}))
		if err != nil {
			return nil, err
		}
		return out.Msg, nil
	}
	if out, err := write(&zero, 1); err != nil || out.GetRevision() != 1 {
		t.Fatalf("insert: %v, %v", out, err)
	}
	if _, err := write(&zero, 2); connect.CodeOf(err) != connect.CodeAborted {
		t.Fatalf("create raced without a conflict: %v", err)
	}
	if out, err := write(&first, 2); err != nil || out.GetRevision() != 2 {
		t.Fatalf("compare and set: %v, %v", out, err)
	}
	if _, err := write(&first, 3); connect.CodeOf(err) != connect.CodeAborted {
		t.Fatalf("stale revision wrote a value: %v", err)
	}
	out, err := server.GetKV(ctx, connect.NewRequest(&pluginv1.ServerServiceGetKVRequest{Key: key}))
	if err != nil || out.Msg.GetRevision() != 2 || !bytes.Equal(out.Msg.GetValueCbor(), []byte{2}) {
		t.Fatalf("get: %v, %v", out, err)
	}
	other := access.WithPrincipal(t.Context(), access.Plugin("other"))
	if _, err := server.GetKV(other, connect.NewRequest(&pluginv1.ServerServiceGetKVRequest{Key: key})); connect.CodeOf(err) != connect.CodeNotFound {
		t.Fatalf("other plugin read private state: %v", err)
	}
}
