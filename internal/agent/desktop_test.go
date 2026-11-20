package agent_test

import (
	"context"
	"os/user"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/cborx"
)

// serveDesktop registers a provider that answers the desktop's methods itself, so
// that the relay is tested without an X server.
func serveDesktop(t *testing.T, client *agent.Client, answer func(method string, args []byte) ([]byte, error)) *provider {
	t.Helper()

	stream := client.HostCalls.Register(context.Background())
	if err := stream.Send(&hostv1.HostCallServiceRegisterRequest{
		Message: &hostv1.HostCallServiceRegisterRequest_Service{Service: &hostv1.HostService{
			Name: agent.DesktopProvider, Methods: agent.DesktopMethods(),
		}},
	}); err != nil {
		t.Fatalf("registering the desktop: %v", err)
	}

	served := &provider{stream: stream, failed: make(chan error, 1)}
	go func() {
		for {
			message, err := stream.Receive()
			if err != nil {
				served.failed <- err
				return
			}
			call := message.GetRequest()
			if call == nil {
				continue
			}
			result := &hostv1.HostCallResult{Id: call.GetId()}
			encoded, err := answer(call.GetMethod(), call.GetArgsCbor())
			if err != nil {
				result.Error = err.Error()
			} else {
				result.ResultCbor = encoded
			}
			if err := stream.Send(&hostv1.HostCallServiceRegisterRequest{
				Message: &hostv1.HostCallServiceRegisterRequest_Result{Result: result},
			}); err != nil {
				served.failed <- err
				return
			}
		}
	}()
	t.Cleanup(func() { _ = stream.CloseRequest() })
	return served
}

// waitDesktop waits for a desktop provider to register.
func waitDesktop(t *testing.T, client *agent.Client) {
	t.Helper()

	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		response, err := client.Desktop.State(context.Background(), connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
		if err != nil {
			t.Fatalf("state: %v", err)
		}
		if response.Msg.GetDesktop().GetAvailable() {
			return
		}
		time.Sleep(5 * time.Millisecond)
	}
	t.Fatal("the desktop provider did not register")
}

func TestDesktopRelay(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	// A host with no desktop says so, and a screenshot is unavailable.
	state, err := client.Desktop.State(context.Background(), connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
	if err != nil {
		t.Fatalf("state: %v", err)
	}
	if state.Msg.GetDesktop().GetAvailable() {
		t.Error("a host with no provider has a desktop")
	}
	if state.Msg.GetDesktop().GetReason() == "" {
		t.Error("a missing desktop says no reason")
	}
	if _, err := client.Desktop.Screenshot(context.Background(), connect.NewRequest(&hostv1.DesktopServiceScreenshotRequest{})); err == nil {
		t.Error("a screenshot worked with no provider")
	} else {
		wantCode(t, err, connect.CodeUnavailable)
	}

	inputs := make(chan agent.DesktopInput, 4)
	serveDesktop(t, client, func(method string, args []byte) ([]byte, error) {
		switch method {
		case agent.DesktopMethodScreenshot:
			var request agent.DesktopScreenshotArgs
			if err := cborx.Unmarshal(args, &request); err != nil {
				return nil, err
			}
			return cborx.Marshal(&agent.DesktopScreenshotResult{
				PNG: []byte("png"), Width: request.Width, Height: request.Height,
			})
		case agent.DesktopMethodInput:
			var input agent.DesktopInput
			if err := cborx.Unmarshal(args, &input); err != nil {
				return nil, err
			}
			inputs <- input
			return nil, nil
		case agent.DesktopMethodClipboardGet:
			return cborx.Marshal(&agent.DesktopClipboard{Text: "copied"})
		case agent.DesktopMethodState:
			return cborx.Marshal(&agent.DesktopStateResult{
				Display: ":1", User: "dev", Width: 1920, Height: 1080, Paused: true,
			})
		default:
			return nil, nil
		}
	})
	waitDesktop(t, client)

	screenshot, err := client.Desktop.Screenshot(context.Background(), connect.NewRequest(&hostv1.DesktopServiceScreenshotRequest{
		Width: 640, Height: 480,
	}))
	if err != nil {
		t.Fatalf("screenshot: %v", err)
	}
	if string(screenshot.Msg.GetPng()) != "png" || screenshot.Msg.GetWidth() != 640 || screenshot.Msg.GetHeight() != 480 {
		t.Errorf("the screenshot is %+v", screenshot.Msg)
	}

	click := &hostv1.DesktopServiceInputRequest{Input: &hostv1.DesktopServiceInputRequest_Click{Click: &hostv1.DesktopClick{
		X: 10, Y: 20, Button: hostv1.MouseButton_MOUSE_BUTTON_RIGHT, Count: 2,
	}}}
	if _, err := client.Desktop.Input(context.Background(), connect.NewRequest(click)); err != nil {
		t.Fatalf("click: %v", err)
	}
	select {
	case input := <-inputs:
		if input.Kind != agent.DesktopInputClick || input.X != 10 || input.Y != 20 || input.Button != "right" || input.Count != 2 {
			t.Errorf("the provider saw %+v", input)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("the click did not arrive")
	}

	scroll := &hostv1.DesktopServiceInputRequest{Input: &hostv1.DesktopServiceInputRequest_Scroll{Scroll: &hostv1.DesktopScroll{
		Direction: hostv1.ScrollDirection_SCROLL_DIRECTION_DOWN, Count: 3,
	}}}
	if _, err := client.Desktop.Input(context.Background(), connect.NewRequest(scroll)); err != nil {
		t.Fatalf("scroll: %v", err)
	}
	if input := <-inputs; input.Kind != agent.DesktopInputScroll || input.Direction != "down" || input.Count != 3 {
		t.Errorf("the provider saw %+v", input)
	}

	clipboard, err := client.Desktop.GetClipboard(context.Background(), connect.NewRequest(&hostv1.DesktopServiceGetClipboardRequest{}))
	if err != nil {
		t.Fatalf("clipboard: %v", err)
	}
	if clipboard.Msg.GetText() != "copied" {
		t.Errorf("the clipboard is %q", clipboard.Msg.GetText())
	}
	if _, err := client.Desktop.SetClipboard(context.Background(), connect.NewRequest(&hostv1.DesktopServiceSetClipboardRequest{Text: "given"})); err != nil {
		t.Fatalf("setting the clipboard: %v", err)
	}

	state, err = client.Desktop.State(context.Background(), connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
	if err != nil {
		t.Fatalf("state: %v", err)
	}
	if !state.Msg.GetDesktop().GetAvailable() || !state.Msg.GetDesktop().GetPaused() ||
		state.Msg.GetDesktop().GetDisplay() != ":1" || state.Msg.GetDesktop().GetWidth() != 1920 {
		t.Errorf("the desktop's state is %+v", state.Msg.GetDesktop())
	}

	version, err := client.Agent.Version(context.Background(), connect.NewRequest(&hostv1.AgentServiceVersionRequest{}))
	if err != nil {
		t.Fatalf("version: %v", err)
	}
	if !version.Msg.GetDesktop().GetAvailable() {
		t.Error("Version does not report the desktop")
	}
}

// TestDesktopOnUserSocket checks who a local program may be to use the desktop.
func TestDesktopOnUserSocket(t *testing.T) {
	t.Parallel()

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}

	daemon := startDaemon(t, func(config *agent.Config) {
		// The desktop belongs to the account running the test, which is the only
		// one a daemon that is not root may act as.
		config.DesktopUser = current.Username
	})
	local := daemon.dialUser(t)

	state, err := local.Desktop.State(context.Background(), connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
	if err != nil {
		t.Fatalf("state: %v", err)
	}
	if state.Msg.GetDesktop().GetUser() != current.Username {
		t.Errorf("the desktop's state belongs to %q", state.Msg.GetDesktop().GetUser())
	}

	// Another account, on a daemon whose desktop belongs to "dev", is refused.
	other := startDaemon(t, func(config *agent.Config) { config.DesktopUser = "dev" })
	if current.Username == "dev" {
		t.Skip("the test runs as the desktop's account")
	}
	_, err = other.dialUser(t).Desktop.State(context.Background(), connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
	wantCode(t, err, connect.CodePermissionDenied)
}

// TestDesktopInputShape checks the error an input with nothing to do gives.
func TestDesktopInputShape(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)
	serveDesktop(t, client, func(string, []byte) ([]byte, error) { return cborx.Marshal(&agent.DesktopStateResult{}) })
	waitDesktop(t, client)

	_, err := client.Desktop.Input(context.Background(), connect.NewRequest(&hostv1.DesktopServiceInputRequest{}))
	wantCode(t, err, connect.CodeInvalidArgument)
}
