package link_test

import (
	"log/slog"
	"testing"
)

// testLogger is a logger that keeps the test's output quiet.
func testLogger(t *testing.T) *slog.Logger {
	t.Helper()
	return slog.New(slog.DiscardHandler)
}
