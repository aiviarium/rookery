package agent

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"sync"
	"syscall"
	"time"

	"github.com/creack/pty"
	"google.golang.org/protobuf/types/known/timestamppb"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

const (
	// sessionRingSize is how much of a session's output is kept for a client
	// that attaches late: 1 MiB.
	sessionRingSize = 1 << 20

	// viewerBuffer is how many output chunks a viewer may fall behind before it
	// is dropped instead of holding the session up.
	viewerBuffer = 256

	// chunkSize is how much a session reads from its terminal at a time.
	chunkSize = 32 << 10
)

// sessionSet is the sessions the daemon has, by id.
type sessionSet struct {
	mu       sync.Mutex
	sessions map[string]*session
}

func newSessionSet() *sessionSet {
	return &sessionSet{sessions: make(map[string]*session)}
}

func (s *sessionSet) add(sess *session) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.sessions[sess.id] = sess
}

func (s *sessionSet) get(id string) (*session, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	sess, ok := s.sessions[id]
	if !ok {
		return nil, fmt.Errorf("%w: no session %s", os.ErrNotExist, id)
	}
	return sess, nil
}

// list returns the sessions, or only the ones an account owns.
func (s *sessionSet) list(owner *account) []*session {
	s.mu.Lock()
	defer s.mu.Unlock()

	sessions := make([]*session, 0, len(s.sessions))
	for _, sess := range s.sessions {
		if owner != nil && sess.account.Name != owner.Name {
			continue
		}
		sessions = append(sessions, sess)
	}
	return sessions
}

func (s *sessionSet) remove(id string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.sessions, id)
}

// closeAll ends every session. The daemon's shutdown reaches it.
func (s *sessionSet) closeAll() error {
	var errs []error
	for _, sess := range s.list(nil) {
		errs = append(errs, sess.close())
	}
	return errors.Join(errs...)
}

// session is a command on a pseudo-terminal, as an account, outliving the calls
// that made and watch it.
type session struct {
	id      string
	account *account
	command string
	cmd     *exec.Cmd
	pty     *os.File
	ring    *ring
	started time.Time

	mu       sync.Mutex
	cols     uint16
	rows     uint16
	viewers  map[*viewer]struct{}
	finished bool
	exitCode int
	done     chan struct{}
}

// chunk is output a viewer has not seen, and where it starts.
type chunk struct {
	offset uint64
	data   []byte
}

// viewer is a client watching a session.
type viewer struct {
	chunks  chan chunk
	dropped bool
	once    sync.Once
}

func (v *viewer) close() {
	v.once.Do(func() { close(v.chunks) })
}

// startSession starts a command on a new terminal.
func startSession(ctx context.Context, acct *account, request *hostv1.SessionServiceCreateRequest) (*session, error) {
	credential, err := acct.credential()
	if err != nil {
		return nil, err
	}

	command := request.GetCommand()
	args := request.GetArgs()
	if command == "" {
		command = acct.loginShell()
		args = []string{"-l"}
	}
	dir := request.GetDir()
	if dir == "" {
		dir = acct.homeDir()
	}

	cols, rows := request.GetCols(), request.GetRows()
	if cols == 0 {
		cols = 80
	}
	if rows == 0 {
		rows = 24
	}

	extra := mergeEnv(request.GetEnv(), "TERM", "xterm-256color")

	// The session outlives the call that made it: it ends when it closes, not
	// when its caller's stream does.
	cmd := exec.CommandContext(context.WithoutCancel(ctx), command, args...) //nolint:gosec // the caller names the command on purpose.
	cmd.Dir = dir
	cmd.Env = acct.environ(extra)
	cmd.SysProcAttr = &syscall.SysProcAttr{Credential: credential}

	terminal, err := pty.StartWithSize(cmd, &pty.Winsize{Cols: uint16(cols), Rows: uint16(rows)}) //nolint:gosec // a terminal has at most 65535 columns. //nolint:gosec // a terminal has at most 65535 columns.
	if err != nil {
		return nil, fmt.Errorf("agent: starting %s for %s: %w", command, acct.Name, translateStartError(err))
	}

	id, err := randomID()
	if err != nil {
		_ = cmd.Process.Kill()
		return nil, err
	}

	sess := &session{
		id:      id,
		account: acct,
		command: formatCommand(command, args),
		cmd:     cmd,
		pty:     terminal,
		ring:    newRing(sessionRingSize),
		started: time.Now(),
		cols:    uint16(cols), //nolint:gosec // a terminal has at most 65535 columns.
		rows:    uint16(rows), //nolint:gosec // a terminal has at most 65535 rows.
		viewers: make(map[*viewer]struct{}),
		done:    make(chan struct{}),
	}
	go sess.pump()
	return sess, nil
}

// pump reads the terminal until it closes, then records how the process ended.
// It is the session's one writer of output, and the owner of cmd.Wait.
func (s *session) pump() {
	buffer := make([]byte, chunkSize)
	for {
		read, err := s.pty.Read(buffer)
		if read > 0 {
			s.emit(buffer[:read])
		}
		if err != nil {
			break
		}
	}

	err := s.cmd.Wait()
	s.finish(exitCodeOf(err))
}

func (s *session) emit(p []byte) {
	s.mu.Lock()
	defer s.mu.Unlock()

	offset := s.ring.total
	s.ring.write(p)

	for v := range s.viewers {
		select {
		case v.chunks <- chunk{offset: offset, data: append([]byte(nil), p...)}:
		default:
			// The viewer reads too slowly to hold a terminal up, so it loses
			// the session instead. It is told so.
			v.dropped = true
			delete(s.viewers, v)
			v.close()
		}
	}
}

func (s *session) finish(code int) {
	s.mu.Lock()
	defer s.mu.Unlock()

	if s.finished {
		return
	}
	s.finished = true
	s.exitCode = code
	close(s.done)
	for v := range s.viewers {
		delete(s.viewers, v)
		v.close()
	}
	_ = s.pty.Close()
}

// attach adds a viewer and returns the output it missed from offset on, the
// offset that output starts at, and whether it wanted output that was already
// overwritten.
func (s *session) attach(offset uint64) (*viewer, []byte, uint64, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()

	output, from, truncated := s.ring.since(offset)
	v := &viewer{chunks: make(chan chunk, viewerBuffer)}
	if s.finished {
		v.close()
	} else {
		s.viewers[v] = struct{}{}
	}
	return v, output, from, truncated
}

func (s *session) detach(v *viewer) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.viewers, v)
	v.close()
}

// write sends a viewer's input to the terminal.
func (s *session) write(p []byte) error {
	if _, err := s.pty.Write(p); err != nil {
		if errors.Is(err, os.ErrClosed) || errors.Is(err, syscall.EIO) {
			return fmt.Errorf("%w: the session has ended", ErrInvalid)
		}
		return fmt.Errorf("agent: writing to the terminal: %w", err)
	}
	return nil
}

// resize sets the terminal's size.
func (s *session) resize(cols, rows uint32) error {
	if cols == 0 || rows == 0 {
		return nil
	}
	if cols > 65535 || rows > 65535 {
		return fmt.Errorf("%w: a terminal has at most 65535 columns and rows", ErrInvalid)
	}

	s.mu.Lock()
	s.cols, s.rows = uint16(cols), uint16(rows) //nolint:gosec // checked above.
	terminal := s.pty
	s.mu.Unlock()

	if err := pty.Setsize(terminal, &pty.Winsize{Cols: uint16(cols), Rows: uint16(rows)}); err != nil { //nolint:gosec // checked above.
		return fmt.Errorf("agent: resizing the terminal: %w", err)
	}
	return nil
}

// signal sends a signal to the session's process group.
func (s *session) signal(name string) error {
	s.mu.Lock()
	finished := s.finished
	s.mu.Unlock()
	if finished {
		return fmt.Errorf("%w: the session has ended", ErrInvalid)
	}

	signal := signalNamed(name)
	if err := syscall.Kill(-s.cmd.Process.Pid, signal); err != nil {
		return fmt.Errorf("agent: sending %s to session %s: %w", name, s.id, err)
	}
	return nil
}

// close hangs the session up, kills it if it does not go, and lets the pump
// finish.
func (s *session) close() error {
	s.mu.Lock()
	finished := s.finished
	s.mu.Unlock()
	if finished {
		return nil
	}

	_ = s.signal("HUP")
	pid := s.cmd.Process.Pid
	go func() {
		select {
		case <-s.done:
		case <-time.After(2 * time.Second):
			_ = syscall.Kill(-pid, syscall.SIGKILL)
		}
	}()
	return nil
}

// info is the session as the RPCs report it.
func (s *session) info() *hostv1.Session {
	s.mu.Lock()
	defer s.mu.Unlock()

	sess := &hostv1.Session{
		Id:          s.id,
		User:        s.account.Name,
		Command:     s.command,
		Pid:         uint32(s.cmd.Process.Pid), //nolint:gosec // a pid is positive.
		Cols:        uint32(s.cols),
		Rows:        uint32(s.rows),
		CreatedAt:   timestamppb.New(s.started),
		Running:     !s.finished,
		ExitCode:    int32(s.exitCode), //nolint:gosec // an exit code is small.
		OutputBytes: s.ring.total,
		Attached:    uint32(len(s.viewers)), //nolint:gosec // a count of clients.
	}
	return sess
}

// exitStatus is how the session ended, for a viewer whose stream is ending.
func (s *session) exitStatus() (finished bool, code int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.finished, s.exitCode
}

// viewerDropped says whether a viewer was dropped for falling behind.
func (s *session) viewerDropped(v *viewer) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	return v.dropped
}

// mergeEnv returns a copy of env with name set.
func mergeEnv(env map[string]string, name, value string) map[string]string {
	merged := make(map[string]string, len(env)+1)
	for key, existing := range env {
		merged[key] = existing
	}
	merged[name] = value
	return merged
}

// randomID makes a session's id.
func randomID() (string, error) {
	var raw [12]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("agent: making an id: %w", err)
	}
	return hex.EncodeToString(raw[:]), nil
}

// exitCodeOf is a process's exit code, with a signal reported as 128 plus its
// number, as a shell does.
func exitCodeOf(err error) int {
	if err == nil {
		return 0
	}
	var exit *exec.ExitError
	if errors.As(err, &exit) {
		if status, ok := exit.Sys().(syscall.WaitStatus); ok {
			if status.Signaled() {
				return 128 + int(status.Signal())
			}
			return status.ExitStatus()
		}
		return exit.ExitCode()
	}
	return 1
}

// formatCommand is a command as listings show it.
func formatCommand(command string, args []string) string {
	formatted := command
	for _, arg := range args {
		formatted += " " + arg
	}
	return formatted
}

// signalNamed resolves a signal's name, without the SIG prefix, and falls back
// to TERM for an unknown one.
func signalNamed(name string) syscall.Signal {
	switch name {
	case "HUP":
		return syscall.SIGHUP
	case "INT":
		return syscall.SIGINT
	case "QUIT":
		return syscall.SIGQUIT
	case "KILL":
		return syscall.SIGKILL
	case "USR1":
		return syscall.SIGUSR1
	case "USR2":
		return syscall.SIGUSR2
	case "STOP":
		return syscall.SIGSTOP
	case "CONT":
		return syscall.SIGCONT
	case "WINCH":
		return syscall.SIGWINCH
	default:
		return syscall.SIGTERM
	}
}

// translateStartError says what a failure to start a command means in the
// caller's terms.
func translateStartError(err error) error {
	switch {
	case errors.Is(err, os.ErrNotExist):
		return fmt.Errorf("%w: no such command: %w", ErrInvalid, err)
	case errors.Is(err, os.ErrPermission):
		return fmt.Errorf("%w: %w", ErrNotPermitted, err)
	default:
		return err
	}
}
