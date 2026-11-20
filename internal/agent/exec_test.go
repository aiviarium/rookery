package agent_test

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// runExec runs a command and collects what it wrote.
func runExec(t *testing.T, client *agent.Client, request *hostv1.ExecServiceRunRequest) (stdout, stderr string, code int32, err error) {
	t.Helper()

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	stream, err := client.Exec.Run(ctx, connect.NewRequest(request))
	if err != nil {
		return "", "", 0, err
	}
	var out, errOut strings.Builder
	for stream.Receive() {
		switch event := stream.Msg().GetEvent().(type) {
		case *hostv1.ExecServiceRunResponse_Stdout:
			out.Write(event.Stdout)
		case *hostv1.ExecServiceRunResponse_Stderr:
			errOut.Write(event.Stderr)
		case *hostv1.ExecServiceRunResponse_Exit:
			code = event.Exit.GetCode()
		}
	}
	if err := stream.Err(); err != nil {
		return out.String(), errOut.String(), code, err
	}
	return out.String(), errOut.String(), code, nil
}

func TestExec(t *testing.T) {
	t.Parallel()

	dir := t.TempDir()
	desktopEnv := filepath.Join(dir, "desktop.env")
	if err := os.WriteFile(desktopEnv, []byte("# the desktop\nGREETING=hi\nDISPLAY=\":1\"\n"), 0o600); err != nil {
		t.Fatalf("writing the desktop's environment: %v", err)
	}

	daemon := startDaemon(t, func(config *agent.Config) { config.DesktopEnvPath = desktopEnv })
	client := daemon.dialRoot(t)

	cases := []struct {
		name       string
		request    *hostv1.ExecServiceRunRequest
		wantStdout string
		wantStderr string
		wantExit   int32
		wantCode   connect.Code
		wantErr    bool
	}{
		{
			name:       "output and exit code",
			request:    &hostv1.ExecServiceRunRequest{Command: "/bin/echo", Args: []string{"hello"}},
			wantStdout: "hello\n",
		},
		{
			name:       "standard error",
			request:    &hostv1.ExecServiceRunRequest{Command: "/bin/sh", Args: []string{"-c", "echo oops >&2; exit 3"}},
			wantStderr: "oops\n",
			wantExit:   3,
		},
		{
			name:       "standard input",
			request:    &hostv1.ExecServiceRunRequest{Command: "/bin/cat", Stdin: []byte("piped")},
			wantStdout: "piped",
		},
		{
			name: "environment and directory",
			request: &hostv1.ExecServiceRunRequest{
				Command: "/bin/sh",
				Args:    []string{"-c", `printf '%s:%s' "$FOO" "$(pwd)"`},
				Env:     map[string]string{"FOO": "bar"},
				Dir:     dir,
			},
			wantStdout: "bar:" + dir,
		},
		{
			name: "the desktop's environment",
			request: &hostv1.ExecServiceRunRequest{
				Command: "/bin/sh",
				Args:    []string{"-c", `printf '%s' "$DISPLAY$GREETING"`},
				Desktop: true,
			},
			wantStdout: ":1hi",
		},
		{
			name:     "no such command",
			request:  &hostv1.ExecServiceRunRequest{Command: "/nonexistent/command"},
			wantErr:  true,
			wantCode: connect.CodeInvalidArgument,
		},
	}

	for _, test := range cases {
		t.Run(test.name, func(t *testing.T) {
			t.Parallel()

			stdout, stderr, code, err := runExec(t, client, test.request)
			if test.wantErr {
				if err == nil {
					t.Fatalf("exec succeeded: stdout %q", stdout)
				}
				var connectErr *connect.Error
				if !errors.As(err, &connectErr) || connectErr.Code() != test.wantCode {
					t.Fatalf("exec failed with %v, want %v", err, test.wantCode)
				}
				return
			}
			if err != nil {
				t.Fatalf("exec: %v", err)
			}
			if stdout != test.wantStdout {
				t.Errorf("stdout = %q, want %q", stdout, test.wantStdout)
			}
			if stderr != test.wantStderr {
				t.Errorf("stderr = %q, want %q", stderr, test.wantStderr)
			}
			if code != test.wantExit {
				t.Errorf("exit code = %d, want %d", code, test.wantExit)
			}
		})
	}
}

// TestExecDesktopWithoutEnvironment checks that a desktop exec on a host with no
// desktop says so rather than running without its display.
func TestExecDesktopWithoutEnvironment(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	_, _, _, err := runExec(t, client, &hostv1.ExecServiceRunRequest{Command: "/bin/true", Desktop: true})
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodeFailedPrecondition {
		t.Fatalf("exec with no desktop failed with %v, want failed_precondition", err)
	}
}

func TestExecTimeout(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	started := time.Now()
	_, _, _, err := runExec(t, client, &hostv1.ExecServiceRunRequest{
		Command:        "/bin/sleep",
		Args:           []string{"30"},
		TimeoutSeconds: 1,
	})
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodeDeadlineExceeded {
		t.Fatalf("a command past its timeout failed with %v, want deadline_exceeded", err)
	}
	if elapsed := time.Since(started); elapsed > 10*time.Second {
		t.Errorf("the timeout took %s", elapsed)
	}
}

// TestExecAsAnotherAccount checks that a test's user may not act as another one:
// a daemon that is not root refuses, and says which account it is.
func TestExecAsAnotherAccount(t *testing.T) {
	t.Parallel()

	if os.Geteuid() == 0 {
		t.Skip("running as root, where any account may be used")
	}

	daemon := startDaemon(t, nil)
	client := daemon.dialRoot(t)

	_, _, _, err := runExec(t, client, &hostv1.ExecServiceRunRequest{
		Command: "/bin/true",
		User:    &hostv1.User{Name: "nobody"},
	})
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodePermissionDenied {
		t.Fatalf("exec as another account failed with %v, want permission_denied", err)
	}
}

// TestExecRefusedOnUserSocket checks that exec is the server's: a local program
// runs its own commands without a daemon.
func TestExecRefusedOnUserSocket(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialUser(t)

	_, _, _, err := runExec(t, client, &hostv1.ExecServiceRunRequest{Command: "/bin/true"})
	var connectErr *connect.Error
	if !errors.As(err, &connectErr) || connectErr.Code() != connect.CodePermissionDenied {
		t.Fatalf("exec on the user socket failed with %v, want permission_denied", err)
	}
}
