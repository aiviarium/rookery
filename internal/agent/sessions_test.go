package agent_test

import (
	"context"
	"os"
	"os/user"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// attachment is a client watching a session.
type attachment struct {
	stream    *connect.BidiStreamForClient[hostv1.SessionServiceAttachRequest, hostv1.SessionServiceAttachResponse]
	responses chan *hostv1.SessionServiceAttachResponse
	errs      chan error
}

// attach subscribes to a session from an offset.
func attach(t *testing.T, client *agent.Client, id string, offset uint64) *attachment {
	t.Helper()

	stream := client.Sessions.Attach(context.Background())
	if err := stream.Send(&hostv1.SessionServiceAttachRequest{Id: id, Offset: offset}); err != nil {
		t.Fatalf("attaching to %s: %v", id, err)
	}

	watching := &attachment{
		stream:    stream,
		responses: make(chan *hostv1.SessionServiceAttachResponse, 256),
		errs:      make(chan error, 1),
	}
	go func() {
		for {
			response, err := stream.Receive()
			if err != nil {
				watching.errs <- err
				return
			}
			watching.responses <- response
		}
	}()
	t.Cleanup(func() { _ = stream.CloseRequest() })
	return watching
}

// send writes to the session's terminal.
func (a *attachment) send(t *testing.T, input string) {
	t.Helper()
	if err := a.stream.Send(&hostv1.SessionServiceAttachRequest{Input: []byte(input)}); err != nil {
		t.Fatalf("sending %q: %v", input, err)
	}
}

// waitFor reads until the output holds want, or fails.
func (a *attachment) waitFor(t *testing.T, want string) string {
	t.Helper()

	var output strings.Builder
	deadline := time.After(20 * time.Second)
	for !strings.Contains(output.String(), want) {
		select {
		case response := <-a.responses:
			output.Write(response.GetOutput())
		case err := <-a.errs:
			if late := lateExit(a); late != nil {
				output.Write(late.GetOutput())
				continue
			}
			t.Fatalf("the stream ended after %q, waiting for %q: %v", output.String(), want, err)
		case <-deadline:
			t.Fatalf("the output is %q after 20s, waiting for %q", output.String(), want)
		}
	}
	return output.String()
}

// waitForExit reads until the session ends, and reports how.
func (a *attachment) waitForExit(t *testing.T) int32 {
	t.Helper()

	deadline := time.After(20 * time.Second)
	for {
		select {
		case response := <-a.responses:
			if response.GetExited() {
				return response.GetExitCode()
			}
		case err := <-a.errs:
			if late := lateExit(a); late != nil {
				return late.GetExitCode()
			}
			t.Fatalf("the stream ended before the session: %v", err)
		case <-deadline:
			t.Fatal("the session did not end within 20s")
		}
	}
}

// lateExit is a response that was already on its way when the stream ended: a
// test reads the two channels together, and the end of the stream may be
// selected before the message it carried.
func lateExit(a *attachment) *hostv1.SessionServiceAttachResponse {
	select {
	case response := <-a.responses:
		if response.GetExited() {
			return response
		}
		// Put it back for the next read, unless it cannot be.
		select {
		case a.responses <- response:
		default:
		}
		return nil
	default:
		return nil
	}
}

// create starts a session.
func create(t *testing.T, client *agent.Client, request *hostv1.SessionServiceCreateRequest) *hostv1.Session {
	t.Helper()

	response, err := client.Sessions.Create(context.Background(), connect.NewRequest(request))
	if err != nil {
		t.Fatalf("creating a session: %v", err)
	}
	return response.Msg.GetSession()
}

func closeSession(t *testing.T, client *agent.Client, id string) {
	t.Helper()
	if _, err := client.Sessions.Close(context.Background(), connect.NewRequest(&hostv1.SessionServiceCloseRequest{Id: id})); err != nil {
		t.Fatalf("closing session %s: %v", id, err)
	}
}

// TestSessionReplays checks a session outlives its viewers: what was written
// while none watched is replayed from an offset when one attaches.
func TestSessionReplays(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	session := create(t, client, &hostv1.SessionServiceCreateRequest{Command: "/bin/cat"})
	defer closeSession(t, client, session.GetId())
	if session.GetRunning() != true || session.GetUser() == "" || session.GetPid() == 0 {
		t.Fatalf("the session is %+v", session)
	}

	watching := attach(t, client, session.GetId(), 0)
	watching.send(t, "hello\n")
	watching.waitFor(t, "hello")
	if err := watching.stream.CloseRequest(); err != nil {
		t.Fatalf("detaching: %v", err)
	}

	// Nothing is watching; the session still has it. A viewer that attaches from
	// the start is replayed what it missed.
	watching = attach(t, client, session.GetId(), 0)
	if output := watching.waitFor(t, "hello"); !strings.Contains(output, "hello") {
		t.Fatalf("the replay is %q", output)
	}

	// A viewer that asks from the end sees only what comes after it.
	list, err := client.Sessions.List(context.Background(), connect.NewRequest(&hostv1.SessionServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(list.Msg.GetSessions()) != 1 {
		t.Fatalf("the listing holds %d sessions, want 1", len(list.Msg.GetSessions()))
	}
	tail := list.Msg.GetSessions()[0].GetOutputBytes()

	watching = attach(t, client, session.GetId(), tail)
	watching.send(t, "later\n")
	if output := watching.waitFor(t, "later"); strings.Contains(output, "hello") {
		t.Errorf("the viewer from the end was replayed %q", output)
	}
}

// TestSessionResize sets a terminal's size and reads it back from the shell.
func TestSessionResize(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	session := create(t, client, &hostv1.SessionServiceCreateRequest{Command: "/bin/sh", Cols: 80, Rows: 24})
	defer closeSession(t, client, session.GetId())

	if _, err := client.Sessions.Resize(context.Background(), connect.NewRequest(&hostv1.SessionServiceResizeRequest{
		Id: session.GetId(), Cols: 100, Rows: 40,
	})); err != nil {
		t.Fatalf("resizing: %v", err)
	}

	watching := attach(t, client, session.GetId(), 0)
	watching.send(t, "stty size\n")
	watching.waitFor(t, "40 100")
}

// TestSessionCloseEnds checks that closing a session hangs it up and that its
// viewers are told how it ended.
func TestSessionCloseEnds(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	session := create(t, client, &hostv1.SessionServiceCreateRequest{Command: "/bin/cat"})
	watching := attach(t, client, session.GetId(), 0)
	watching.send(t, "there\n")
	watching.waitFor(t, "there")

	closeSession(t, client, session.GetId())
	watching.waitForExit(t)

	if _, err := client.Sessions.Signal(context.Background(), connect.NewRequest(&hostv1.SessionServiceSignalRequest{
		Id: session.GetId(), Signal: "TERM",
	})); err == nil {
		t.Error("a closed session can still be signalled")
	}
}

// TestSessionSignals checks that a signal name reaches the command.
func TestSessionSignals(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	session := create(t, client, &hostv1.SessionServiceCreateRequest{Command: "/bin/cat"})
	watching := attach(t, client, session.GetId(), 0)
	watching.send(t, "still there\n")
	watching.waitFor(t, "still there")

	if _, err := client.Sessions.Signal(context.Background(), connect.NewRequest(&hostv1.SessionServiceSignalRequest{
		Id: session.GetId(), Signal: "TERM",
	})); err != nil {
		t.Fatalf("signalling: %v", err)
	}
	if code := watching.waitForExit(t); code != 128+15 {
		t.Errorf("the session ended with %d, want %d", code, 128+15)
	}
}

// TestSessionOnUserSocket checks what a local program may do with sessions: its
// own, as its own account, and not another account's.
func TestSessionOnUserSocket(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	root := daemon.dialRoot(t)
	local := daemon.dialUser(t)

	current, err := user.Current()
	if err != nil {
		t.Fatalf("the test's account: %v", err)
	}

	mine := create(t, local, &hostv1.SessionServiceCreateRequest{Command: "/bin/cat"})
	defer closeSession(t, local, mine.GetId())
	if mine.GetUser() != current.Username {
		t.Errorf("the session runs as %q, want %q", mine.GetUser(), current.Username)
	}

	list, err := local.Sessions.List(context.Background(), connect.NewRequest(&hostv1.SessionServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	if len(list.Msg.GetSessions()) != 1 || list.Msg.GetSessions()[0].GetId() != mine.GetId() {
		t.Errorf("a local program lists %v, want only its own session", list.Msg.GetSessions())
	}

	_, err = local.Sessions.Create(context.Background(), connect.NewRequest(&hostv1.SessionServiceCreateRequest{
		Command: "/bin/cat",
		User:    &hostv1.User{Name: "root"},
	}))
	wantCode(t, err, connect.CodePermissionDenied)

	_, err = local.Sessions.Close(context.Background(), connect.NewRequest(&hostv1.SessionServiceCloseRequest{Id: "somebody-elses"}))
	wantCode(t, err, connect.CodeNotFound)

	if os.Geteuid() != 0 {
		// Another account's session is another account's: a root daemon makes
		// one, and the local program neither sees nor closes it.
		t.Log("not root: another account's session is not tested")
		return
	}

	theirs := create(t, root, &hostv1.SessionServiceCreateRequest{Command: "/bin/cat", User: &hostv1.User{Name: "nobody"}})
	defer closeSession(t, root, theirs.GetId())

	list, err = local.Sessions.List(context.Background(), connect.NewRequest(&hostv1.SessionServiceListRequest{}))
	if err != nil {
		t.Fatalf("listing: %v", err)
	}
	for _, session := range list.Msg.GetSessions() {
		if session.GetId() == theirs.GetId() {
			t.Error("a local program lists another account's session")
		}
	}

	_, err = local.Sessions.Close(context.Background(), connect.NewRequest(&hostv1.SessionServiceCloseRequest{Id: theirs.GetId()}))
	wantCode(t, err, connect.CodeNotFound)
}

// TestSessionByUnknownID checks the error a missing session gives.
func TestSessionByUnknownID(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	_, err := client.Sessions.Close(context.Background(), connect.NewRequest(&hostv1.SessionServiceCloseRequest{Id: "nope"}))
	wantCode(t, err, connect.CodeNotFound)
}
