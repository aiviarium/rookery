package jobs

import (
	"context"
	"fmt"
	"strings"
	"sync"
	"time"

	"github.com/aiviarium/rookery/internal/store"
)

// writeTimeout bounds writing a line after the job's own context has ended.
const writeTimeout = 10 * time.Second

// Log is a job's log: a line at a time, masked on the way in.
//
// A job is given secrets, and its output may show one by accident. The masker
// replaces each with asterisks before the line is stored, so a secret never
// reaches the database, a follower or the CLI.
type Log struct {
	ctx    context.Context
	store  *store.Store
	jobID  string
	masker *Masker

	mu sync.Mutex
}

// NewLog returns a job's log. A nil masker masks nothing. The context is the
// job's, and a line still goes in after that context has ended.
func NewLog(ctx context.Context, opened *store.Store, jobID string, masker *Masker) *Log {
	return &Log{ctx: ctx, store: opened, jobID: jobID, masker: masker}
}

// Write adds a line, masked.
func (l *Log) Write(line string) error {
	return l.WriteLine(line)
}

// Printf adds a formatted line, masked.
func (l *Log) Printf(format string, args ...any) error {
	return l.WriteLine(fmt.Sprintf(format, args...))
}

// WriteLine adds one line, masked, and strips the newline it may carry.
func (l *Log) WriteLine(line string) error {
	line = strings.TrimRight(l.mask(line), "\n")
	ctx, cancel := context.WithTimeout(context.WithoutCancel(l.ctx), writeTimeout)
	defer cancel()
	if _, err := l.store.Queries().InsertJobLine(ctx, store.InsertJobLineParams{JobID: l.jobID, Line: line}); err != nil {
		return fmt.Errorf("jobs: writing a line of %s: %w", l.jobID, err)
	}
	return nil
}

// AddSecret masks a secret in subsequent log lines and the job's final error.
func (l *Log) AddSecret(value string) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.masker == nil {
		l.masker = NewMasker()
	}
	l.masker.Add(value)
}

// mask is a line with the secrets replaced.
func (l *Log) mask(line string) string {
	l.mu.Lock()
	defer l.mu.Unlock()
	if l.masker == nil {
		return line
	}
	return l.masker.Mask(line)
}

// Masker hides secret values in a job's output.
type Masker struct {
	secrets []string
}

// NewMasker returns a masker for the secrets a job was given. Empty values are
// ignored: masking "" would rewrite every line.
func NewMasker(secrets ...string) *Masker {
	masker := &Masker{}
	for _, secret := range secrets {
		if secret != "" {
			masker.secrets = append(masker.secrets, secret)
		}
	}
	return masker
}

// Add adds a secret to hide.
func (m *Masker) Add(secret string) {
	if secret != "" {
		m.secrets = append(m.secrets, secret)
	}
}

// Mask replaces every secret with asterisks.
func (m *Masker) Mask(line string) string {
	for _, secret := range m.secrets {
		line = strings.ReplaceAll(line, secret, strings.Repeat("*", len(secret)))
	}
	return line
}
