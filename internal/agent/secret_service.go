package agent

import (
	"context"
	"fmt"
	"io"
	"os"
	"strings"
	"syscall"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

type secretService struct {
	dir string
}

func (s *secretService) Get(ctx context.Context, request *connect.Request[hostv1.SecretServiceGetRequest]) (*connect.Response[hostv1.SecretServiceGetResponse], error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	if caller.role != roleUser {
		return nil, denied("agent: host secrets are for local programs")
	}
	name := request.Msg.GetName()
	if name == "" || name == "." || name == ".." || strings.ContainsAny(name, "/\\\x00") {
		return nil, invalid("agent: the secret name must be a single file name")
	}

	root, err := os.OpenRoot(s.dir)
	if err != nil {
		return nil, toConnect(err)
	}
	defer func() { _ = root.Close() }()
	// A malformed grant could point at a FIFO. Open it nonblocking and check
	// both type and ownership on the open file before reading.
	file, err := root.OpenFile(name, os.O_RDONLY|syscall.O_NONBLOCK, 0)
	if err != nil {
		return nil, toConnect(err)
	}
	defer func() { _ = file.Close() }()
	info, err := file.Stat()
	if err != nil {
		return nil, toConnect(err)
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	mode := info.Mode()
	if !mode.IsRegular() || (mode.Perm() != 0o400 && mode.Perm() != 0o600) || !ok || stat.Uid != caller.uid {
		return nil, denied("agent: the secret is not granted to this account")
	}
	const maxSecretBytes = 1 << 20
	data, err := io.ReadAll(io.LimitReader(file, maxSecretBytes+1))
	if err != nil {
		return nil, toConnect(err)
	}
	if len(data) > maxSecretBytes {
		return nil, toConnect(fmt.Errorf("%w: the secret exceeds %d bytes", ErrInvalid, maxSecretBytes))
	}
	return connect.NewResponse(&hostv1.SecretServiceGetResponse{Value: data}), nil
}
