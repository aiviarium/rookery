//go:build e2e

package e2e_test

import (
	"context"
	"net/http"
	"testing"

	"connectrpc.com/connect"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/internal/boxcli"
	"github.com/aiviarium/rookery/internal/cborx"
)

func checkBox(ctx context.Context, t *testing.T, client *http.Client, site, token string, studio hostFixture) {
	t.Helper()
	const service = "example/echo"
	ctx, cancel := context.WithCancel(ctx)
	provider := make(chan error, 1)
	options := boxcli.Options{Socket: studio.userSocket}
	go func() {
		provider <- boxcli.Run(ctx, options, []string{"serve", service, "--", "/bin/sh", "-c", `
			read -r payload
			printf '{"method":"%s","request":%s}\n' "$1" "$payload"
		`, "fixture"})
	}()
	defer func() {
		cancel()
		if err := <-provider; err != nil {
			t.Errorf("box serve: %v", err)
		}
	}()

	calls := rookeryv1connect.NewHostCallServiceClient(client, site, connect.WithGRPC())
	if err := await(ctx, func() bool {
		list, err := calls.List(ctx, bearer(token, &rookeryv1.HostCallServiceListRequest{Host: "studio"}))
		if err != nil {
			return false
		}
		for _, registered := range list.Msg.GetServices() {
			if registered.GetName() == service {
				return true
			}
		}
		return false
	}); err != nil {
		t.Fatalf("waiting for the local host service: %v", err)
	}
	args, err := cborx.Marshal("hello")
	if err != nil {
		t.Fatal(err)
	}
	response, err := calls.Call(ctx, bearer(token, &rookeryv1.HostCallServiceCallRequest{
		Host: "studio", Service: service, Method: "greet", ArgsCbor: args,
	}))
	if err != nil {
		t.Fatal(err)
	}
	var answer map[string]string
	if err := cborx.Unmarshal(response.Msg.GetResultCbor(), &answer); err != nil {
		t.Fatal(err)
	}
	if answer["method"] != "greet" || answer["request"] != "hello" {
		t.Fatalf("box service response: %v", answer)
	}

	topic := "user/" + studio.user + "/ready"
	if err := boxcli.Run(ctx, options, []string{"publish", topic, `{"ready":true}`, "--keep", "log"}); err != nil {
		t.Fatal(err)
	}
	topics := rookeryv1connect.NewTopicServiceClient(client, site, connect.WithGRPC())
	stream, err := topics.Subscribe(ctx, bearer(token, &rookeryv1.TopicServiceSubscribeRequest{
		Topic: "host/studio/" + topic, Consumer: "e2e",
	}))
	if err != nil {
		t.Fatal(err)
	}
	defer func() { _ = stream.Close() }()
	if !stream.Receive() {
		t.Fatalf("host channel did not reach the server topic: %v", stream.Err())
	}
	var payload struct {
		Ready bool `cbor:"ready"`
	}
	if err := cborx.Unmarshal(stream.Msg().GetMessage().GetPayloadCbor(), &payload); err != nil || !payload.Ready {
		t.Fatalf("host channel payload: %+v, %v", payload, err)
	}
}
