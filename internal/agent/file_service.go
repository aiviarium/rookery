package agent

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"syscall"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// fileService serves FileService: a host's files.
//
// Files are for the server: a local program's account may read and write its own
// files without rookeryd, so the user socket refuses these calls.
type fileService struct {
	mutations sync.Mutex
}

func (h *fileService) Read(ctx context.Context, request *connect.Request[hostv1.FileServiceReadRequest]) (*connect.Response[hostv1.FileServiceReadResponse], error) {
	acct, err := fileAccount(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return nil, err
	}
	if credential, err := acct.credential(); err != nil {
		return nil, toConnect(err)
	} else if credential != nil {

		return runFileWorker(ctx, "read", acct, credential, request.Msg, &hostv1.FileServiceReadResponse{})
	}

	path := resolvePath(acct, request.Msg.GetPath())

	file, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NONBLOCK, 0) //nolint:gosec // the server named the path.
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: opening %s: %w", path, err))
	}
	defer func() { _ = file.Close() }()

	info, err := file.Stat()
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: %s: %w", path, err))
	}
	if !info.Mode().IsRegular() {
		return nil, invalid("agent: %s is not a regular file", path)
	}

	size := uint64(info.Size()) //nolint:gosec // a size is not negative.
	offset := request.Msg.GetOffset()
	if offset > size {
		return nil, invalid("agent: the offset %d is past the end of %s", offset, path)
	}
	if _, err := file.Seek(int64(offset), io.SeekStart); err != nil { //nolint:gosec // bounded by the file's size.
		return nil, toConnect(fmt.Errorf("agent: seeking in %s: %w", path, err))
	}

	length := request.Msg.GetLength()
	if length == 0 || length > size-offset {
		length = size - offset
	}
	// Callers can continue a ranged read, including when a file is large.
	length = min(length, 4<<20)
	data := make([]byte, length)
	read, err := io.ReadFull(file, data)
	if err != nil && !errors.Is(err, io.ErrUnexpectedEOF) && !errors.Is(err, io.EOF) {
		return nil, toConnect(fmt.Errorf("agent: reading %s: %w", path, err))
	}

	return connect.NewResponse(&hostv1.FileServiceReadResponse{
		Data:   data[:read],
		Offset: offset,
		Total:  size,
	}), nil
}

func (h *fileService) Write(ctx context.Context, request *connect.Request[hostv1.FileServiceWriteRequest]) (*connect.Response[hostv1.FileServiceWriteResponse], error) {
	acct, err := fileAccount(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return nil, err
	}
	if credential, err := acct.credential(); err != nil {
		return nil, toConnect(err)
	} else if credential != nil {
		h.mutations.Lock()
		defer h.mutations.Unlock()
		return runFileWorker(ctx, "write", acct, credential, request.Msg, &hostv1.FileServiceWriteResponse{})
	}
	h.mutations.Lock()
	defer h.mutations.Unlock()
	path := resolvePath(acct, request.Msg.GetPath())

	revision := request.Msg.GetRevision()
	if revision != "" {
		current, err := revisionOf(path)
		if errors.Is(err, os.ErrNotExist) {
			return nil, toConnect(fmt.Errorf("%w: %s is not there", ErrRevisionConflict, path))
		}
		if err != nil {
			return nil, toConnect(err)
		}
		if current != revision {
			return nil, toConnect(fmt.Errorf("%w: %s is at %s", ErrRevisionConflict, path, current))
		}
	}

	if strings.HasSuffix(request.Msg.GetPath(), "/") {
		if revision != "" || len(request.Msg.GetData()) > 0 {
			return nil, invalid("agent: a directory write has no content or revision")
		}
		mode := os.FileMode(request.Msg.GetMode() & 0o777)
		if mode == 0 {
			mode = 0o755
		}
		if err := os.Mkdir(path, mode); err != nil {
			return nil, toConnect(fmt.Errorf("agent: creating %s: %w", path, err))
		}
		return connect.NewResponse(&hostv1.FileServiceWriteResponse{}), nil
	}

	flags := os.O_WRONLY | os.O_CREATE | os.O_TRUNC | syscall.O_NONBLOCK
	if request.Msg.GetCreate() {
		flags = os.O_WRONLY | os.O_CREATE | os.O_EXCL
	}
	mode := os.FileMode(request.Msg.GetMode() & 0o777)
	if mode == 0 {
		mode = 0o644
	}

	file, err := os.OpenFile(path, flags, mode) //nolint:gosec // the server named the path and the mode.
	if err != nil {
		if errors.Is(err, os.ErrExist) {
			return nil, toConnect(fmt.Errorf("%w: %s", ErrExists, path))
		}
		return nil, toConnect(fmt.Errorf("agent: writing %s: %w", path, err))
	}
	if _, err := file.Write(request.Msg.GetData()); err != nil {
		_ = file.Close()
		return nil, toConnect(fmt.Errorf("agent: writing %s: %w", path, err))
	}
	if err := file.Close(); err != nil {
		return nil, toConnect(fmt.Errorf("agent: closing %s: %w", path, err))
	}
	digest := sha256.Sum256(request.Msg.GetData())
	written := "sha256:" + hex.EncodeToString(digest[:])
	return connect.NewResponse(&hostv1.FileServiceWriteResponse{
		Revision: written,
		Size:     uint64(len(request.Msg.GetData())),
	}), nil
}

func (h *fileService) List(ctx context.Context, request *connect.Request[hostv1.FileServiceListRequest]) (*connect.Response[hostv1.FileServiceListResponse], error) {
	acct, err := fileAccount(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return nil, err
	}
	if credential, err := acct.credential(); err != nil {
		return nil, toConnect(err)
	} else if credential != nil {

		return runFileWorker(ctx, "list", acct, credential, request.Msg, &hostv1.FileServiceListResponse{})
	}

	path := resolvePath(acct, request.Msg.GetPath())

	entries, err := os.ReadDir(path)
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: listing %s: %w", path, err))
	}

	response := &hostv1.FileServiceListResponse{Entries: make([]*hostv1.File, 0, len(entries))}
	for _, entry := range entries {
		info, err := entry.Info()
		if err != nil {
			// A file that went away between the listing and the stat is left
			// out: it is not there any more.
			if errors.Is(err, os.ErrNotExist) {
				continue
			}
			return nil, toConnect(fmt.Errorf("agent: %s: %w", filepath.Join(path, entry.Name()), err))
		}
		response.Entries = append(response.Entries, describeFile(filepath.Join(path, entry.Name()), info))
	}
	return connect.NewResponse(response), nil
}

func (h *fileService) Stat(ctx context.Context, request *connect.Request[hostv1.FileServiceStatRequest]) (*connect.Response[hostv1.FileServiceStatResponse], error) {
	acct, err := fileAccount(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return nil, err
	}
	if credential, err := acct.credential(); err != nil {
		return nil, toConnect(err)
	} else if credential != nil {

		return runFileWorker(ctx, "stat", acct, credential, request.Msg, &hostv1.FileServiceStatResponse{})
	}

	path := resolvePath(acct, request.Msg.GetPath())

	info, err := os.Stat(path)
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: %s: %w", path, err))
	}
	file := describeFile(path, info)
	if !info.IsDir() {
		if file.Revision, err = revisionOf(path); err != nil {
			return nil, toConnect(err)
		}
	}
	return connect.NewResponse(&hostv1.FileServiceStatResponse{File: file}), nil
}

func (h *fileService) Delete(ctx context.Context, request *connect.Request[hostv1.FileServiceDeleteRequest]) (*connect.Response[hostv1.FileServiceDeleteResponse], error) {
	acct, err := fileAccount(ctx, request.Msg.GetUser().GetName())
	if err != nil {
		return nil, err
	}
	if credential, err := acct.credential(); err != nil {
		return nil, toConnect(err)
	} else if credential != nil {
		h.mutations.Lock()
		defer h.mutations.Unlock()
		return runFileWorker(ctx, "delete", acct, credential, request.Msg, &hostv1.FileServiceDeleteResponse{})
	}
	h.mutations.Lock()
	defer h.mutations.Unlock()
	path := resolvePath(acct, request.Msg.GetPath())

	info, err := os.Lstat(path)
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: %s: %w", path, err))
	}
	if revision := request.Msg.GetRevision(); revision != "" && !info.IsDir() {
		current, err := revisionOf(path)
		if err != nil {
			return nil, toConnect(err)
		}
		if current != revision {
			return nil, toConnect(fmt.Errorf("%w: %s is at %s", ErrRevisionConflict, path, current))
		}
	}
	if info.IsDir() && request.Msg.GetRecursive() {
		if err := os.RemoveAll(path); err != nil {
			return nil, toConnect(fmt.Errorf("agent: removing %s: %w", path, err))
		}
	} else if err := os.Remove(path); err != nil {
		return nil, toConnect(fmt.Errorf("agent: removing %s: %w", path, err))
	}
	return connect.NewResponse(&hostv1.FileServiceDeleteResponse{}), nil
}

// account resolves the call's account, and refuses the user socket.
func fileAccount(ctx context.Context, name string) (*account, error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	if caller.role != roleRoot {
		return nil, denied("agent: files are for the server, not for local programs")
	}
	acct, _, err := accountFor(ctx, name)
	return acct, err
}

// resolvePath makes a relative path relative to the account's home, so that the
// server's and a user's idea of "notes.txt" are the same file.
func resolvePath(acct *account, path string) string {
	if path == "" {
		return acct.homeDir()
	}
	if strings.HasPrefix(path, "~/") {
		return filepath.Join(acct.homeDir(), path[2:])
	}
	if filepath.IsAbs(path) {
		return filepath.Clean(path)
	}
	return filepath.Join(acct.homeDir(), path)
}

// describeFile is a path's state, as the RPCs report it.
func describeFile(path string, info os.FileInfo) *hostv1.File {
	return &hostv1.File{
		Path:       path,
		Name:       info.Name(),
		Dir:        info.IsDir(),
		Size:       uint64(info.Size()), //nolint:gosec // a size is not negative. //nolint:gosec // a size is not negative.
		Mode:       uint32(info.Mode().Perm()),
		ModifiedAt: timestamppb.New(info.ModTime()),
	}
}

// revisionOf is a file's revision: the SHA-256 of its content.
func revisionOf(path string) (string, error) {
	file, err := os.OpenFile(path, os.O_RDONLY|syscall.O_NONBLOCK, 0) //nolint:gosec // the server named the path.
	if err != nil {
		return "", fmt.Errorf("agent: reading %s: %w", path, err)
	}
	defer func() { _ = file.Close() }()

	info, err := file.Stat()
	if err != nil {
		return "", fmt.Errorf("agent: stating %s: %w", path, err)
	}
	if !info.Mode().IsRegular() {
		return "", fmt.Errorf("%w: %s is not a regular file", ErrInvalid, path)
	}
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", fmt.Errorf("agent: hashing %s: %w", path, err)
	}
	return "sha256:" + hex.EncodeToString(hash.Sum(nil)), nil
}
