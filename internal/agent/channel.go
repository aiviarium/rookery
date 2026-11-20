package agent

import (
	"encoding/binary"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"go.etcd.io/bbolt"
	"google.golang.org/protobuf/types/known/timestamppb"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

// defaultChannelLogLimit is how many messages a topic kept as a log holds. The
// oldest goes when the next one arrives.
const defaultChannelLogLimit = 10_000

// maxChannelPayload bounds a message, so that a broken publisher cannot fill the
// disk with one write.
const maxChannelPayload = 1 << 20

// The channel store's buckets.
var (
	messagesBucket = []byte("messages")
	indexBucket    = []byte("index")
	countsBucket   = []byte("counts")
	metaBucket     = []byte("meta")
	nextSequence   = []byte("next")
)

// storedMessage is a message as the store keeps it.
type storedMessage struct {
	Topic   string    `cbor:"topic"`
	Payload []byte    `cbor:"payload"`
	At      time.Time `cbor:"at"`
}

// channelStore is the host's channel: a bbolt file of the messages kept, and the
// followers that are watching now.
//
// One sequence counts every message, kept or not, so a follower's cursor is a
// single number across all topics. A message that is not kept is streamed to the
// followers that are there and then forgotten.
type channelStore struct {
	db       *bbolt.DB
	logLimit int

	// mu orders publishing, so that sequences and the followers that see them
	// are ordered the same way, and guards latest and followers.
	mu        sync.Mutex
	latest    uint64
	followers map[*follower]struct{}
	closed    bool
}

// follower is a stream waiting for messages with a prefix.
type follower struct {
	prefix   string
	messages chan *hostv1.ChannelMessage
}

// openChannelStore opens or creates the channel's file.
func openChannelStore(path string, logLimit int) (*channelStore, error) {
	if logLimit <= 0 {
		logLimit = defaultChannelLogLimit
	}
	if dir := filepath.Dir(path); dir != "." {
		if err := os.MkdirAll(dir, 0o750); err != nil {
			return nil, fmt.Errorf("agent: creating the channel store's directory: %w", err)
		}
	}
	db, err := bbolt.Open(path, 0o600, &bbolt.Options{Timeout: 5 * time.Second})
	if err != nil {
		return nil, fmt.Errorf("agent: opening the channel store %s: %w", path, err)
	}

	store := &channelStore{db: db, logLimit: logLimit, followers: make(map[*follower]struct{})}
	err = db.Update(func(tx *bbolt.Tx) error {
		for _, name := range [][]byte{messagesBucket, indexBucket, countsBucket, metaBucket} {
			if _, err := tx.CreateBucketIfNotExists(name); err != nil {
				return err
			}
		}
		if raw := tx.Bucket(metaBucket).Get(nextSequence); len(raw) == 8 {
			store.latest = binary.BigEndian.Uint64(raw)
		}
		return nil
	})
	if err != nil {
		_ = db.Close()
		return nil, fmt.Errorf("agent: preparing the channel store: %w", err)
	}
	return store, nil
}

// Close closes the file and ends the followers.
func (s *channelStore) Close() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil
	}
	s.closed = true
	for f := range s.followers {
		close(f.messages)
		delete(s.followers, f)
	}
	return s.db.Close()
}

// publish stores a message as keep asks and streams it to the followers.
func (s *channelStore) publish(topic string, keep hostv1.ChannelKeep, payload []byte, at time.Time) (*hostv1.ChannelMessage, error) {
	if len(payload) > maxChannelPayload {
		return nil, fmt.Errorf("%w: a message is at most %d bytes", ErrInvalid, maxChannelPayload)
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil, fmt.Errorf("%w: the channel store is closed", ErrUnavailable)
	}

	encoded, err := cborx.Marshal(storedMessage{Topic: topic, Payload: payload, At: at})
	if err != nil {
		return nil, err
	}

	var sequence uint64
	err = s.db.Update(func(tx *bbolt.Tx) error {
		sequence = s.latest + 1
		if err := tx.Bucket(metaBucket).Put(nextSequence, encodeSequence(sequence)); err != nil {
			return err
		}

		if keep == hostv1.ChannelKeep_CHANNEL_KEEP_LAST || keep == hostv1.ChannelKeep_CHANNEL_KEEP_LOG {
			messages := tx.Bucket(messagesBucket)
			index, err := tx.Bucket(indexBucket).CreateBucketIfNotExists([]byte(topic))
			if err != nil {
				return err
			}
			if err := messages.Put(encodeSequence(sequence), encoded); err != nil {
				return err
			}
			if err := index.Put(encodeSequence(sequence), nil); err != nil {
				return err
			}
			limit := s.logLimit
			if keep == hostv1.ChannelKeep_CHANNEL_KEEP_LAST {
				limit = 1
			}
			if err := prune(tx, messages, index, topic, limit); err != nil {
				return err
			}
		}

		s.latest = sequence
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("agent: storing a message on %s: %w", topic, err)
	}

	message := &hostv1.ChannelMessage{
		Topic:       topic,
		Sequence:    sequence,
		PayloadCbor: payload,
		PublishedAt: timestamppb.New(at),
	}
	for f := range s.followers {
		if !hasTopicPrefix(topic, f.prefix) {
			continue
		}
		select {
		case f.messages <- message:
		default:
			// A follower that does not read loses what does not fit; the ones
			// kept are still in the store for it to read again.
		}
	}
	return message, nil
}

// prune drops a topic's oldest messages until only limit are left. The count is
// kept beside the topic because a bucket's own statistics are the ones on disk,
// not the ones a transaction has written yet.
func prune(tx *bbolt.Tx, messages, index *bbolt.Bucket, topic string, limit int) error {
	counts := tx.Bucket(countsBucket)
	count := uint64(1)
	if raw := counts.Get([]byte(topic)); len(raw) == 8 {
		count = binary.BigEndian.Uint64(raw) + 1
	}

	cursor := index.Cursor()
	for count > uint64(limit) { //nolint:gosec // a limit is positive.
		sequence, _ := cursor.First()
		if sequence == nil {
			count = 0
			break
		}
		if err := messages.Delete(sequence); err != nil {
			return err
		}
		if err := cursor.Delete(); err != nil {
			return err
		}
		count--
	}
	return counts.Put([]byte(topic), encodeSequence(count))
}

// read returns a topic's kept messages after a sequence, oldest first.
func (s *channelStore) read(topic string, after uint64, limit uint32) ([]*hostv1.ChannelMessage, error) {
	messages := make([]*hostv1.ChannelMessage, 0, limit)
	err := s.db.View(func(tx *bbolt.Tx) error {
		index := tx.Bucket(indexBucket).Bucket([]byte(topic))
		if index == nil {
			return nil
		}
		stored := tx.Bucket(messagesBucket)

		cursor := index.Cursor()
		for key, _ := cursor.Seek(encodeSequence(after + 1)); key != nil; key, _ = cursor.Next() {
			if limit > 0 && len(messages) >= int(limit) {
				return nil
			}
			message, err := decodeMessage(stored, key)
			if err != nil {
				return err
			}
			messages = append(messages, message)
		}
		return nil
	})
	if err != nil {
		return nil, fmt.Errorf("agent: reading %s: %w", topic, err)
	}
	return messages, nil
}

// follow registers a follower for a prefix and returns it with the last sequence
// published when it registered. Messages after that sequence are the ones the
// follower is streamed, so a caller reads the store up to it first and misses
// nothing.
func (s *channelStore) follow(prefix string) (*follower, uint64, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.closed {
		return nil, 0, fmt.Errorf("%w: the channel store is closed", ErrUnavailable)
	}

	f := &follower{prefix: prefix, messages: make(chan *hostv1.ChannelMessage, 256)}
	s.followers[f] = struct{}{}
	return f, s.latest, nil
}

// unfollow ends a follower's stream.
func (s *channelStore) unfollow(f *follower) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if _, ok := s.followers[f]; !ok {
		return
	}
	delete(s.followers, f)
	close(f.messages)
}

// decodeMessage reads one stored message.
func decodeMessage(stored *bbolt.Bucket, key []byte) (*hostv1.ChannelMessage, error) {
	var message storedMessage
	if err := cborx.Unmarshal(stored.Get(key), &message); err != nil {
		return nil, err
	}
	return &hostv1.ChannelMessage{
		Topic:       message.Topic,
		Sequence:    binary.BigEndian.Uint64(key),
		PayloadCbor: message.Payload,
		PublishedAt: timestamppb.New(message.At),
	}, nil
}

// encodeSequence is a sequence as a bbolt key: eight bytes, big endian, so that
// keys sort as numbers.
func encodeSequence(sequence uint64) []byte {
	key := make([]byte, 8)
	binary.BigEndian.PutUint64(key, sequence)
	return key
}

// hasTopicPrefix says whether a topic is under a follower's prefix. A prefix
// that ends with a slash matches its topics, and a bare one matches the topic
// itself and everything under it.
func hasTopicPrefix(topic, prefix string) bool {
	if prefix == "" {
		return true
	}
	if strings.HasSuffix(prefix, "/") {
		return strings.HasPrefix(topic, prefix)
	}
	return topic == prefix || strings.HasPrefix(topic, prefix+"/")
}

// validatePrefix checks a prefix's shape, which is a topic's, or the start of one.
func validatePrefix(prefix string) error {
	if len(prefix) > 256 {
		return fmt.Errorf("%w: a prefix is at most 256 characters", ErrInvalid)
	}
	trimmed := strings.TrimSuffix(prefix, "/")
	if trimmed == "" {
		return nil
	}
	return validateSegments(prefix, trimmed)
}

// validateTopic checks a topic's shape: two segments or more, of letters,
// digits, dots, dashes and underscores. PLUGIN/TOPIC and user/ACCOUNT/TOPIC are
// both that.
func validateTopic(topic string) error {
	if topic == "" || len(topic) > 256 {
		return fmt.Errorf("%w: a topic is one to 256 characters", ErrInvalid)
	}
	if len(strings.Split(topic, "/")) < 2 {
		return fmt.Errorf("%w: the topic %q is PLUGIN/TOPIC or user/ACCOUNT/TOPIC", ErrInvalid, topic)
	}
	return validateSegments(topic, topic)
}

// validateSegments checks each segment's characters, naming the whole topic when
// one is wrong.
func validateSegments(name, segments string) error {
	for _, segment := range strings.Split(segments, "/") {
		if segment == "" || segment == "." || segment == ".." || len(segment) > 64 {
			return fmt.Errorf("%w: %q has an empty, dotted or too long segment", ErrInvalid, name)
		}
		for _, char := range segment {
			switch {
			case char >= 'a' && char <= 'z', char >= 'A' && char <= 'Z', char >= '0' && char <= '9':
			case char == '.', char == '-', char == '_':
			default:
				return fmt.Errorf("%w: %q has %q in it", ErrInvalid, name, char)
			}
		}
	}
	return nil
}
