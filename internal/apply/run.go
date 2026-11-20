package apply

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"sync"
	"time"
)

// waitDelay is how long a cancelled run may take to die before it is abandoned
// and its pipes are closed anyway.
const waitDelay = 10 * time.Second

// runPlaybook runs ansible-playbook in the work directory with every line of its
// output masked and sent to the sink. The process is in its own process group, so
// a cancelled context kills the SSH and Python it started too.
func (r *Runner) runPlaybook(ctx context.Context, workDir string, sink LineSink, masker *Masker) error {
	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()
	//nolint:gosec // the executable is the release's ansible-playbook, named by configuration.
	command := exec.CommandContext(runCtx, r.playbook(),
		"-i", filepath.Join(workDir, "inventory.yml"),
		filepath.Join(workDir, "site.yml"),
		"-e", "@"+filepath.Join(workDir, "vars.json"),
	)
	command.Dir = workDir
	// The child's environment is named, not inherited: nothing the server has,
	// a token or a secret among it, reaches Ansible.
	command.Env = []string{
		"PATH=" + os.Getenv("PATH"),
		"HOME=" + filepath.Join(workDir, "home"),
		"LANG=C.UTF-8",
		"TMPDIR=" + workDir,
		"ANSIBLE_CONFIG=" + filepath.Join(workDir, "ansible.cfg"),
		"ANSIBLE_FORCE_COLOR=0",
		"ANSIBLE_NOCOLOR=1",
		"PYTHONUNBUFFERED=1",
	}
	if r.Socket != "" {
		command.Env = append(command.Env, "ROOKERY_SOCKET="+r.Socket)
	}
	command.SysProcAttr = processGroup()
	// A cancelled context kills the group, not only the process ansible-playbook
	// is, and ssh sessions with it.
	command.Cancel = func() error { return killGroup(command) }
	command.WaitDelay = waitDelay

	output, err := command.StdoutPipe()
	if err != nil {
		return fmt.Errorf("apply: reading the run's output: %w", err)
	}
	stderr, err := command.StderrPipe()
	if err != nil {
		return fmt.Errorf("apply: reading the run's errors: %w", err)
	}

	if err := command.Start(); err != nil {
		return fmt.Errorf("apply: starting %s: %w", r.playbook(), err)
	}

	lines := make(chan string)
	reading := make(chan error, 2)
	var scanners sync.WaitGroup
	for _, pipe := range []io.Reader{output, stderr} {
		scanners.Add(1)
		go func(pipe io.Reader) {
			defer scanners.Done()
			if err := scanLines(pipe, lines); err != nil {
				reading <- err
				cancel()
			}
		}(pipe)
	}
	go func() {
		scanners.Wait()
		close(lines)
		close(reading)
	}()

	var sinkErr error
	for line := range lines {
		if sinkErr != nil {
			continue // drain, so the child is never blocked on a full pipe
		}
		if err := sink.Line(ctx, masker.Mask(line)); err != nil {
			sinkErr = fmt.Errorf("apply: writing a line of the run's output: %w", err)
			cancel()
		}
	}
	for err := range reading {
		if err != nil && sinkErr == nil {
			sinkErr = err
		}
	}

	waitErr := command.Wait()
	if err := ctx.Err(); err != nil {
		return fmt.Errorf("apply: the run was cancelled: %w", err)
	}
	if sinkErr != nil {
		return sinkErr
	}
	if waitErr != nil {
		var exit *exec.ExitError
		if errors.As(waitErr, &exit) {
			return fmt.Errorf("apply: ansible-playbook failed with %s", exit.String())
		}
		return fmt.Errorf("apply: waiting for ansible-playbook: %w", waitErr)
	}
	return nil
}

// scanLines sends the pipe's lines to lines, and its reading error to reading.
func scanLines(pipe io.Reader, lines chan<- string) error {
	scanner := bufio.NewScanner(pipe)
	// An Ansible line can be long: a traceback or a file's contents in a diff.
	scanner.Buffer(make([]byte, 0, 64*1024), 1024*1024)
	for scanner.Scan() {
		lines <- scanner.Text()
	}
	return scanner.Err()
}

// Masker hides secret values in a run's output: each one becomes "***".
type Masker struct {
	// values are the secrets, longest first, so that one that contains another
	// is masked whole.
	values []string
}

// mask is what a secret becomes.
const mask = "***"

// NewMasker builds a masker for the values, skipping the empty ones.
func NewMasker(values ...string) *Masker {
	masker := &Masker{}
	for _, value := range values {
		if value == "" || slices.Contains(masker.values, value) {
			continue
		}
		masker.values = append(masker.values, value)
		// Ansible may render a value as JSON or across output lines.
		encoded, err := json.Marshal(value)
		if err == nil {
			escaped := string(encoded[1 : len(encoded)-1])
			if escaped != value && !slices.Contains(masker.values, escaped) {
				masker.values = append(masker.values, escaped)
			}
		}
		if strings.ContainsAny(value, "\r\n") {
			for _, line := range strings.FieldsFunc(value, func(r rune) bool { return r == '\r' || r == '\n' }) {
				if !slices.Contains(masker.values, line) {
					masker.values = append(masker.values, line)
				}
			}
		}
	}
	sort.Slice(masker.values, func(i, j int) bool {
		if len(masker.values[i]) != len(masker.values[j]) {
			return len(masker.values[i]) > len(masker.values[j])
		}
		return masker.values[i] < masker.values[j]
	})
	return masker
}

// Mask returns the line with every secret in it replaced.
func (m *Masker) Mask(line string) string {
	for _, value := range m.values {
		if strings.Contains(line, value) {
			line = strings.ReplaceAll(line, value, mask)
		}
	}
	return line
}

// Len is how many secrets the masker hides.
func (m *Masker) Len() int { return len(m.values) }
