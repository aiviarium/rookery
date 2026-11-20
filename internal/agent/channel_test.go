package agent

import (
	"strings"
	"testing"
	"time"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// TestChannelStoreLogLimit checks a log topic keeps only its newest messages.
func TestChannelStoreLogLimit(t *testing.T) {
	t.Parallel()

	store, err := openChannelStore(t.TempDir()+"/channel.db", 5)
	if err != nil {
		t.Fatalf("opening the store: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	for i := 0; i < 12; i++ {
		if _, err := store.publish("example/log", hostv1.ChannelKeep_CHANNEL_KEEP_LOG, []byte{byte(i)}, time.Now()); err != nil {
			t.Fatalf("publishing %d: %v", i, err)
		}
	}

	messages, err := store.read("example/log", 0, 0)
	if err != nil {
		t.Fatalf("reading: %v", err)
	}
	if len(messages) != 5 {
		t.Fatalf("the log holds %d messages, want 5", len(messages))
	}
	for i, message := range messages {
		if want := byte(7 + i); message.GetPayloadCbor()[0] != want {
			t.Errorf("message %d is %d, want %d", i, message.GetPayloadCbor()[0], want)
		}
	}
}

// TestChannelStoreLast checks a last topic keeps one message, replacing it.
func TestChannelStoreLast(t *testing.T) {
	t.Parallel()

	store, err := openChannelStore(t.TempDir()+"/channel.db", 0)
	if err != nil {
		t.Fatalf("opening the store: %v", err)
	}
	t.Cleanup(func() { _ = store.Close() })

	for i := 0; i < 3; i++ {
		if _, err := store.publish("example/last", hostv1.ChannelKeep_CHANNEL_KEEP_LAST, []byte{byte(i)}, time.Now()); err != nil {
			t.Fatalf("publishing %d: %v", i, err)
		}
	}

	messages, err := store.read("example/last", 0, 0)
	if err != nil {
		t.Fatalf("reading: %v", err)
	}
	if len(messages) != 1 || messages[0].GetPayloadCbor()[0] != 2 {
		t.Fatalf("the topic holds %v, want only the last message", messages)
	}
}

// TestChannelStoreSequences checks the sequence counts every message, kept or
// not, and survives reopening the file.
func TestChannelStoreSequences(t *testing.T) {
	t.Parallel()

	path := t.TempDir() + "/channel.db"
	store, err := openChannelStore(path, 0)
	if err != nil {
		t.Fatalf("opening the store: %v", err)
	}

	first, err := store.publish("example/one", hostv1.ChannelKeep_CHANNEL_KEEP_NONE, []byte("a"), time.Now())
	if err != nil {
		t.Fatalf("publishing: %v", err)
	}
	second, err := store.publish("example/two", hostv1.ChannelKeep_CHANNEL_KEEP_LOG, []byte("b"), time.Now())
	if err != nil {
		t.Fatalf("publishing: %v", err)
	}
	if first.GetSequence() != 1 || second.GetSequence() != 2 {
		t.Errorf("sequences are %d and %d, want 1 and 2", first.GetSequence(), second.GetSequence())
	}
	if err := store.Close(); err != nil {
		t.Fatalf("closing: %v", err)
	}

	reopened, err := openChannelStore(path, 0)
	if err != nil {
		t.Fatalf("reopening the store: %v", err)
	}
	t.Cleanup(func() { _ = reopened.Close() })

	third, err := reopened.publish("example/two", hostv1.ChannelKeep_CHANNEL_KEEP_LOG, []byte("c"), time.Now())
	if err != nil {
		t.Fatalf("publishing after reopening: %v", err)
	}
	if third.GetSequence() != 3 {
		t.Errorf("the sequence after reopening is %d, want 3", third.GetSequence())
	}
}

// TestValidateTopic checks the shapes a topic may and may not have.
func TestValidateTopic(t *testing.T) {
	t.Parallel()

	for _, topic := range []string{
		"example/progress",
		"user/dev/notes",
		"user/dev/a.b-c_d/thing",
		"a/b",
	} {
		if err := validateTopic(topic); err != nil {
			t.Errorf("%q was refused: %v", topic, err)
		}
	}
	for _, topic := range []string{
		"",
		"example",
		"example/",
		"/example",
		"example//twice",
		"example/has space",
		"example/../etc",
		"example/" + strings.Repeat("x", 300),
	} {
		if err := validateTopic(topic); err == nil {
			t.Errorf("%q was accepted", topic)
		}
	}
}

// TestHasTopicPrefix checks what a follower's prefix matches.
func TestHasTopicPrefix(t *testing.T) {
	t.Parallel()

	cases := []struct {
		prefix string
		topic  string
		want   bool
	}{
		{"", "example/x", true},
		{"example/", "example/x", true},
		{"example/", "example/x/y", true},
		{"example/", "examples/x", false},
		{"example", "example", true},
		{"example", "example/x", true},
		{"example", "examples", false},
		{"user/dev/", "user/dev/notes", true},
		{"user/dev/", "user/other/notes", false},
	}
	for _, test := range cases {
		if got := hasTopicPrefix(test.topic, test.prefix); got != test.want {
			t.Errorf("hasTopicPrefix(%q, %q) = %v, want %v", test.topic, test.prefix, got, test.want)
		}
	}
}
