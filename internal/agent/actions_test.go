package agent_test

import (
	"context"
	"errors"
	"os"
	"os/user"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/cborx"
)

// actionsFile is a file of user actions for a test.
const actionsFile = `{
  "actions": [
    {
      "name": "say",
      "label": "Say hello",
      "group": "example",
      "user": "root",
      "description": "Says hello to someone",
      "command": "printf '%s' \"$ROOKERY_ARG_WHO\"",
      "arguments": [
        {"name": "who", "label": "Who", "required": false, "default": "world"}
      ]
    },
    {
      "name": "fail",
      "label": "Fail",
      "group": "example",
      "user": "operator",
      "command": "echo oops >&2; exit 4"
    },
    {
      "name": "needs",
      "label": "Needs an argument",
      "group": "example",
      "user": "desktop",
      "command": "printf '%s' \"$ROOKERY_ARG_WHAT\"",
      "arguments": [{"name": "what", "label": "What", "required": true}]
    }
  ]
}
`

// writeActions writes a file of actions and returns its path.
func writeActions(t *testing.T, content string) string {
	t.Helper()

	path := filepath.Join(t.TempDir(), "actions.json")
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatalf("writing the actions: %v", err)
	}
	return path
}

// runAction runs an action and collects what it wrote.
func runAction(t *testing.T, client *agent.Client, request *hostv1.ActionServiceRunRequest) (stdout, stderr string, code int32, err error) {
	t.Helper()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	stream, err := client.Actions.Run(ctx, connect.NewRequest(request))
	if err != nil {
		return "", "", 0, err
	}
	var out, errOut strings.Builder
	for stream.Receive() {
		switch event := stream.Msg().GetEvent().(type) {
		case *hostv1.ActionServiceRunResponse_Stdout:
			out.Write(event.Stdout)
		case *hostv1.ActionServiceRunResponse_Stderr:
			errOut.Write(event.Stderr)
		case *hostv1.ActionServiceRunResponse_Exit:
			code = event.Exit.GetCode()
		}
	}
	if err := stream.Err(); err != nil {
		return out.String(), errOut.String(), code, err
	}
	return out.String(), errOut.String(), code, nil
}

func TestActions(t *testing.T) {
	t.Parallel()

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}

	path := writeActions(t, actionsFile)
	daemon := startDaemon(t, func(config *agent.Config) { config.ActionsPath = path })
	client := daemon.dialRoot(t)

	t.Run("list", func(t *testing.T) {
		t.Parallel()

		response, err := client.Actions.List(context.Background(), connect.NewRequest(&hostv1.ActionServiceListRequest{}))
		if err != nil {
			t.Fatalf("listing: %v", err)
		}
		if len(response.Msg.GetActions()) != 3 {
			t.Fatalf("the host has %d actions, want 3", len(response.Msg.GetActions()))
		}
		say := response.Msg.GetActions()[0]
		if say.GetName() != "fail" && say.GetName() != "say" {
			t.Errorf("the actions are not in group order: %v", response.Msg.GetActions())
		}
		for _, action := range response.Msg.GetActions() {
			if !action.GetOnHost() {
				t.Errorf("%s is not marked for users", action.GetName())
			}
			if action.GetName() == "say" {
				if action.GetLabel() != "Say hello" || action.GetUser() != hostv1.ActionUser_ACTION_USER_ROOT {
					t.Errorf("say is %+v", action)
				}
				if len(action.GetArguments()) != 1 || action.GetArguments()[0].GetDefaultValue() != "world" {
					t.Errorf("say's arguments are %v", action.GetArguments())
				}
			}
		}
	})

	t.Run("run with an argument", func(t *testing.T) {
		t.Parallel()

		// The arguments are CBOR, as everything dynamic is.
		args, err := marshalArgs(t, map[string]any{"who": "there"})
		if err != nil {
			t.Fatalf("encoding the arguments: %v", err)
		}
		stdout, _, code, err := runAction(t, client, &hostv1.ActionServiceRunRequest{
			Name: "say", ArgsCbor: args, User: &hostv1.User{Name: current.Username},
		})
		if err != nil {
			t.Fatalf("running: %v", err)
		}
		if stdout != "there" || code != 0 {
			t.Errorf("say printed %q and exited %d, want there and 0", stdout, code)
		}
	})

	t.Run("run with a default", func(t *testing.T) {
		t.Parallel()

		stdout, _, _, err := runAction(t, client, &hostv1.ActionServiceRunRequest{
			Name: "say", User: &hostv1.User{Name: current.Username},
		})
		if err != nil {
			t.Fatalf("running: %v", err)
		}
		if stdout != "world" {
			t.Errorf("say printed %q, want the default world", stdout)
		}
	})

	t.Run("failure", func(t *testing.T) {
		t.Parallel()

		_, stderr, code, err := runAction(t, client, &hostv1.ActionServiceRunRequest{
			Name: "fail", User: &hostv1.User{Name: current.Username},
		})
		if err != nil {
			t.Fatalf("running: %v", err)
		}
		if stderr != "oops\n" || code != 4 {
			t.Errorf("fail printed %q on stderr and exited %d", stderr, code)
		}
	})

	t.Run("required argument", func(t *testing.T) {
		t.Parallel()

		_, _, _, err := runAction(t, client, &hostv1.ActionServiceRunRequest{
			Name: "needs", User: &hostv1.User{Name: current.Username},
		})
		var connectErr *connect.Error
		if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodeInvalidArgument {
			t.Fatalf("running without a required argument failed with %v", err)
		}
	})

	t.Run("unknown argument", func(t *testing.T) {
		t.Parallel()

		args, err := marshalArgs(t, map[string]any{"nonsense": 1})
		if err != nil {
			t.Fatalf("encoding the arguments: %v", err)
		}
		_, _, _, err = runAction(t, client, &hostv1.ActionServiceRunRequest{
			Name: "say", ArgsCbor: args, User: &hostv1.User{Name: current.Username},
		})
		var connectErr *connect.Error
		if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodeInvalidArgument {
			t.Fatalf("an unknown argument failed with %v", err)
		}
	})

	t.Run("unknown action", func(t *testing.T) {
		t.Parallel()

		stream, err := client.Actions.Run(context.Background(), connect.NewRequest(&hostv1.ActionServiceRunRequest{Name: "nothing"}))
		if err == nil {
			stream.Receive()
			err = stream.Err()
		}
		wantCode(t, err, connect.CodeNotFound)
	})
}

// TestActionsReload checks that applying a host again takes effect without
// restarting the daemon.
func TestActionsReload(t *testing.T) {
	t.Parallel()

	path := writeActions(t, `{"actions": [{"name": "before", "user": "root", "command": "true"}]}`)
	daemon := startDaemon(t, func(config *agent.Config) { config.ActionsPath = path })
	client := daemon.dialRoot(t)

	names := func() []string {
		response, err := client.Actions.List(context.Background(), connect.NewRequest(&hostv1.ActionServiceListRequest{}))
		if err != nil {
			t.Fatalf("listing: %v", err)
		}
		found := make([]string, 0, len(response.Msg.GetActions()))
		for _, action := range response.Msg.GetActions() {
			found = append(found, action.GetName())
		}
		return found
	}

	if got := names(); len(got) != 1 || got[0] != "before" {
		t.Fatalf("the actions are %v, want before", got)
	}

	if err := os.WriteFile(path, []byte(`{"actions": [{"name": "after", "user": "root", "command": "true"}]}`), 0o600); err != nil {
		t.Fatalf("rewriting the actions: %v", err)
	}
	// A file that changes within the same second may keep its modification
	// time; an apply rewrites the file much later than that.
	later := time.Now().Add(time.Second)
	if err := os.Chtimes(path, later, later); err != nil {
		t.Fatalf("touching the actions: %v", err)
	}

	if got := names(); len(got) != 1 || got[0] != "after" {
		t.Fatalf("the actions are %v, want after", got)
	}
}

// TestActionsOnUserSocket checks that a local program runs the actions that are
// open to users, as itself.
func TestActionsOnUserSocket(t *testing.T) {
	t.Parallel()

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}
	path := writeActions(t, actionsFile)
	daemon := startDaemon(t, func(config *agent.Config) { config.ActionsPath = path })
	local := daemon.dialUser(t)

	if _, err := local.Actions.List(context.Background(), connect.NewRequest(&hostv1.ActionServiceListRequest{})); err != nil {
		t.Fatalf("listing: %v", err)
	}

	stdout, _, _, err := runAction(t, local, &hostv1.ActionServiceRunRequest{Name: "say"})
	if err != nil {
		t.Fatalf("running: %v", err)
	}
	if stdout != "world" {
		t.Errorf("say printed %q", stdout)
	}

	_, _, _, err = runAction(t, local, &hostv1.ActionServiceRunRequest{
		Name: "say", User: &hostv1.User{Name: "root"},
	})
	wantCode(t, err, connect.CodePermissionDenied)

	_, err = local.Actions.List(context.Background(), connect.NewRequest(&hostv1.ActionServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing as %s: %v", current.Username, err)
	}
}

// TestActionsBrokenFile checks that a file the apply got wrong says so.
func TestActionsBrokenFile(t *testing.T) {
	t.Parallel()

	path := writeActions(t, `{"actions": [{"name": "broken", "command": "true"}]}`)
	daemon := startDaemon(t, func(config *agent.Config) { config.ActionsPath = path })
	client := daemon.dialRoot(t)

	_, err := client.Actions.List(context.Background(), connect.NewRequest(&hostv1.ActionServiceListRequest{}))
	if err == nil {
		t.Fatal("a file with no user was accepted")
	}
}

// marshalArgs encodes an action's arguments, as a client would.
func marshalArgs(t *testing.T, args map[string]any) ([]byte, error) {
	t.Helper()
	return cborx.Marshal(args)
}
