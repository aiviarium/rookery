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

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

// fileService serves FileService: a host's files.
//
// Files are for the server: a local program's account may read and write its own
// files without rookeryd, so the user socket refuses these calls.
type fileService struct {
	server *Server
}

func (h *fileService) Read(ctx context.Context, request *connect.Request[hostv1.FileServiceReadRequest]) (*connect.Response[hostv1.FileServiceReadResponse], error) {
	acct, err := h.account(ctx)
	if err != nil {
		return nil, err
	}
	path := resolvePath(acct, request.Msg.GetPath())

	file, err := os.Open(path) //nolint:gosec // the server named the path.
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: opening %s: %w", path, err))
	}
	defer func() { _ = file.Close() }()

	info, err := file.Stat()
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: %s: %w", path, err))
	}
	if info.IsDir() {
		return nil, invalid("agent: %s is a directory", path)
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
	acct, err := h.account(ctx)
	if err != nil {
		return nil, err
	}
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

	flags := os.O_WRONLY | os.O_CREATE | os.O_TRUNC
	if request.Msg.GetCreate() {
		flags = os.O_WRONLY | os.O_CREATE | os.O_EXCL
	}
	mode := os.FileMode(request.Msg.GetMode())
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
	if err := h.own(path, acct); err != nil {
		return nil, toConnect(err)
	}

	written, err := revisionOf(path)
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.FileServiceWriteResponse{
		Revision: written,
		Size:     uint64(len(request.Msg.GetData())),
	}), nil
}

func (h *fileService) List(ctx context.Context, request *connect.Request[hostv1.FileServiceListRequest]) (*connect.Response[hostv1.FileServiceListResponse], error) {
	acct, err := h.account(ctx)
	if err != nil {
		return nil, err
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
	acct, err := h.account(ctx)
	if err != nil {
		return nil, err
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
	acct, err := h.account(ctx)
	if err != nil {
		return nil, err
	}
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
func (h *fileService) account(ctx context.Context) (*account, error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	if caller.role != roleRoot {
		return nil, denied("agent: files are for the server, not for local programs")
	}
	acct, _, err := accountFor(ctx, "")
	return acct, err
}

// own chowns a file to the account it was written for, so that a write as a user
// leaves a file the user owns.
func (h *fileService) own(path string, acct *account) error {
	if os.Geteuid() != 0 {
		return nil
	}
	if err := os.Chown(path, int(acct.UID), int(acct.GID)); err != nil {
		return fmt.Errorf("agent: giving %s to %s: %w", path, acct.Name, err)
	}
	return nil
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
	file, err := os.Open(path) //nolint:gosec // the server named the path.
	if err != nil {
		return "", fmt.Errorf("agent: reading %s: %w", path, err)
	}
	defer func() { _ = file.Close() }()

	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", fmt.Errorf("agent: hashing %s: %w", path, err)
	}
	return "sha256:" + hex.EncodeToString(hash.Sum(nil)), nil
}
