package agent_test

import (
	"bytes"
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// fileClient is a root socket's file service, with a directory of its own.
type fileClient struct {
	*agent.Client
	dir string
}

func newFileClient(t *testing.T) fileClient {
	t.Helper()

	daemon := startDaemon(t, nil)
	return fileClient{Client: daemon.dialRoot(t), dir: t.TempDir()}
}

func (f fileClient) write(t *testing.T, path string, data []byte, revision string, create bool) *hostv1.FileServiceWriteResponse {
	t.Helper()

	response, err := f.Files.Write(context.Background(), connect.NewRequest(&hostv1.FileServiceWriteRequest{
		Path:     path,
		Data:     data,
		Revision: revision,
		Create:   create,
	}))
	if err != nil {
		t.Fatalf("writing %s: %v", path, err)
	}
	return response.Msg
}

func TestFiles(t *testing.T) {
	t.Parallel()

	files := newFileClient(t)
	path := filepath.Join(files.dir, "notes.txt")

	written := files.write(t, path, []byte("first"), "", false)
	if written.GetSize() != 5 {
		t.Errorf("the write reported %d bytes, want 5", written.GetSize())
	}
	if !strings.HasPrefix(written.GetRevision(), "sha256:") {
		t.Errorf("the revision is %q, want a sha256", written.GetRevision())
	}

	t.Run("read", func(t *testing.T) {
		t.Parallel()

		response, err := files.Files.Read(context.Background(), connect.NewRequest(&hostv1.FileServiceReadRequest{Path: path}))
		if err != nil {
			t.Fatalf("reading: %v", err)
		}
		if string(response.Msg.GetData()) != "first" {
			t.Errorf("read %q, want first", response.Msg.GetData())
		}
		if response.Msg.GetTotal() != 5 {
			t.Errorf("total is %d, want 5", response.Msg.GetTotal())
		}

		offset, err := files.Files.Read(context.Background(), connect.NewRequest(&hostv1.FileServiceReadRequest{
			Path: path, Offset: 1, Length: 3,
		}))
		if err != nil {
			t.Fatalf("reading from an offset: %v", err)
		}
		if string(offset.Msg.GetData()) != "irs" || offset.Msg.GetOffset() != 1 {
			t.Errorf("read %q at %d, want irs at 1", offset.Msg.GetData(), offset.Msg.GetOffset())
		}
	})

	t.Run("stat", func(t *testing.T) {
		t.Parallel()

		response, err := files.Files.Stat(context.Background(), connect.NewRequest(&hostv1.FileServiceStatRequest{Path: path}))
		if err != nil {
			t.Fatalf("stat: %v", err)
		}
		if response.Msg.GetFile().GetRevision() != written.GetRevision() {
			t.Errorf("stat's revision is %q, the write's is %q",
				response.Msg.GetFile().GetRevision(), written.GetRevision())
		}
		if response.Msg.GetFile().GetSize() != 5 || response.Msg.GetFile().GetDir() {
			t.Errorf("stat = %+v", response.Msg.GetFile())
		}
	})

	t.Run("list", func(t *testing.T) {
		t.Parallel()

		response, err := files.Files.List(context.Background(), connect.NewRequest(&hostv1.FileServiceListRequest{Path: files.dir}))
		if err != nil {
			t.Fatalf("listing: %v", err)
		}
		found := false
		for _, entry := range response.Msg.GetEntries() {
			if entry.GetName() == "notes.txt" {
				found = true
				if entry.GetSize() != 5 {
					t.Errorf("the listing says %d bytes, want 5", entry.GetSize())
				}
			}
		}
		if !found {
			t.Errorf("the listing does not hold notes.txt: %v", response.Msg.GetEntries())
		}
	})

	t.Run("compare and swap", func(t *testing.T) {
		t.Parallel()

		path := filepath.Join(files.dir, "swapped.txt")
		written := files.write(t, path, []byte("one"), "", false)
		if files.write(t, path, []byte("two"), written.GetRevision(), false).GetSize() != 3 {
			t.Error("the write did not report its size")
		}

		_, err := files.Files.Write(context.Background(), connect.NewRequest(&hostv1.FileServiceWriteRequest{
			Path: path, Data: []byte("three"), Revision: written.GetRevision(),
		}))
		wantCode(t, err, connect.CodeAborted)

		_, err = files.Files.Write(context.Background(), connect.NewRequest(&hostv1.FileServiceWriteRequest{
			Path: filepath.Join(files.dir, "not-there.txt"), Data: []byte("x"), Revision: "sha256:00",
		}))
		wantCode(t, err, connect.CodeAborted)

		_, err = files.Files.Write(context.Background(), connect.NewRequest(&hostv1.FileServiceWriteRequest{
			Path: path, Data: []byte("four"), Create: true,
		}))
		wantCode(t, err, connect.CodeAlreadyExists)
	})

	t.Run("delete", func(t *testing.T) {
		t.Parallel()

		path := filepath.Join(files.dir, "gone.txt")
		files.write(t, path, []byte("bye"), "", false)
		if _, err := files.Files.Delete(context.Background(), connect.NewRequest(&hostv1.FileServiceDeleteRequest{Path: path})); err != nil {
			t.Fatalf("deleting: %v", err)
		}
		if _, err := os.Stat(path); !errors.Is(err, os.ErrNotExist) {
			t.Errorf("the file is still there: %v", err)
		}

		_, err := files.Files.Read(context.Background(), connect.NewRequest(&hostv1.FileServiceReadRequest{Path: path}))
		wantCode(t, err, connect.CodeNotFound)
	})

	t.Run("relative paths are the account's", func(t *testing.T) {
		t.Parallel()

		// The daemon's account is the test's user, whose home is real. A made-up
		// name in it is not there, which is the point: the path was resolved
		// against that home rather than the daemon's working directory.
		_, err := files.Files.Stat(context.Background(), connect.NewRequest(&hostv1.FileServiceStatRequest{
			Path: ".rookery-test-not-there-" + t.Name(),
		}))
		wantCode(t, err, connect.CodeNotFound)
	})
}

// TestFilesRefusedOnUserSocket checks that files are the server's.
func TestFilesRefusedOnUserSocket(t *testing.T) {
	t.Parallel()

	daemon := startDaemon(t, nil)
	client := daemon.dialUser(t)

	_, err := client.Files.List(context.Background(), connect.NewRequest(&hostv1.FileServiceListRequest{Path: t.TempDir()}))
	wantCode(t, err, connect.CodePermissionDenied)
}

// TestFileWriteCreatesDirectories is not what it sounds like: writing into a
// directory that is not there fails rather than making it, so a typo in a path
// is an error and not a tree.
func TestFileWriteIntoMissingDirectory(t *testing.T) {
	t.Parallel()

	files := newFileClient(t)
	path := filepath.Join(files.dir, "missing", "notes.txt")

	_, err := files.Files.Write(context.Background(), connect.NewRequest(&hostv1.FileServiceWriteRequest{
		Path: path, Data: bytes.Repeat([]byte("x"), 10), Create: true,
	}))
	wantCode(t, err, connect.CodeNotFound)
}
