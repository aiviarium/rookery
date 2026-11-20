package hosts

import (
	"context"
	"fmt"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/proto"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/link"
)

// FileService performs file operations under the permissions of a host account.
type FileService struct{ *Runtime }

func (s *FileService) client(ctx context.Context, host, account string) (*link.Client, func(), *hostv1.User, error) {
	record, err := s.hosts.authorizeHost(ctx, host, access.Use, CapabilityFiles)
	if err != nil {
		return nil, nil, nil, err
	}
	user, _, err := s.account(ctx, record, account)
	if err != nil {
		return nil, nil, nil, err
	}
	client, release, err := s.hosts.deps.Links.Use(ctx, host)
	return client, release, user, err
}

// Read returns bytes from a file at an offset.
func (s *FileService) Read(ctx context.Context, request *connect.Request[rookeryv1.FileServiceReadRequest]) (*connect.Response[rookeryv1.FileServiceReadResponse], error) {
	input := request.Msg.GetFile()
	if input == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: file request is required", access.ErrInvalid))
	}

	client, release, user, err := s.client(ctx, request.Msg.GetHost(), input.GetUser().GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	remote := proto.CloneOf(input)
	remote.User = user
	response, err := client.Files.Read(ctx, connect.NewRequest(remote))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.FileServiceReadResponse{Result: response.Msg}), nil
}

// Write writes content and returns its new revision.
func (s *FileService) Write(ctx context.Context, request *connect.Request[rookeryv1.FileServiceWriteRequest]) (*connect.Response[rookeryv1.FileServiceWriteResponse], error) {
	input := request.Msg.GetFile()
	if input == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: file request is required", access.ErrInvalid))
	}
	if input.GetPath() == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: file path is required", access.ErrInvalid))
	}
	client, release, user, err := s.client(ctx, request.Msg.GetHost(), input.GetUser().GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	remote := proto.CloneOf(input)
	remote.User = user
	response, err := client.Files.Write(ctx, connect.NewRequest(remote))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.FileServiceWriteResponse{Result: response.Msg}), nil
}

// List returns the entries in a directory.
func (s *FileService) List(ctx context.Context, request *connect.Request[rookeryv1.FileServiceListRequest]) (*connect.Response[rookeryv1.FileServiceListResponse], error) {
	input := request.Msg.GetFile()
	if input == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: file request is required", access.ErrInvalid))
	}

	client, release, user, err := s.client(ctx, request.Msg.GetHost(), input.GetUser().GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	remote := proto.CloneOf(input)
	remote.User = user
	response, err := client.Files.List(ctx, connect.NewRequest(remote))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.FileServiceListResponse{Result: response.Msg}), nil
}

// Stat reports a path's metadata and revision.
func (s *FileService) Stat(ctx context.Context, request *connect.Request[rookeryv1.FileServiceStatRequest]) (*connect.Response[rookeryv1.FileServiceStatResponse], error) {
	input := request.Msg.GetFile()
	if input == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: file request is required", access.ErrInvalid))
	}

	client, release, user, err := s.client(ctx, request.Msg.GetHost(), input.GetUser().GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	remote := proto.CloneOf(input)
	remote.User = user
	response, err := client.Files.Stat(ctx, connect.NewRequest(remote))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.FileServiceStatResponse{Result: response.Msg}), nil
}

// Delete removes a file or directory.
func (s *FileService) Delete(ctx context.Context, request *connect.Request[rookeryv1.FileServiceDeleteRequest]) (*connect.Response[rookeryv1.FileServiceDeleteResponse], error) {
	input := request.Msg.GetFile()
	if input == nil {
		return nil, access.ToConnect(fmt.Errorf("%w: file request is required", access.ErrInvalid))
	}
	if input.GetPath() == "" {
		return nil, access.ToConnect(fmt.Errorf("%w: file path is required", access.ErrInvalid))
	}
	client, release, user, err := s.client(ctx, request.Msg.GetHost(), input.GetUser().GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	defer release()
	remote := proto.CloneOf(input)
	remote.User = user
	response, err := client.Files.Delete(ctx, connect.NewRequest(remote))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.FileServiceDeleteResponse{Result: response.Msg}), nil
}
