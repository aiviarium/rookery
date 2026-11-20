package server

import (
	"context"
	"errors"
	"fmt"
	"io"
	"path/filepath"
	"strings"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// DialNode carries a plugin's connection to a managed host through rookeryd.
func (p *PluginServer) DialNode(ctx context.Context, upstream *connect.BidiStream[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse]) error {
	if _, err := requirePlugin(ctx); err != nil {
		return err
	}
	first, err := upstream.Receive()
	if err != nil {
		return err
	}
	host := first.GetNode()
	if host == "" || strings.ContainsAny(host, "/?#") {
		return access.ToConnect(access.ErrInvalid)
	}
	if node, provisioner := access.ProvisionerNode(ctx); provisioner {
		if node == "" || node != host {
			return access.ToConnect(access.ErrDenied)
		}
	} else if err := p.checker.Check(ctx, access.MustPrincipal(ctx), access.HostURI(host), access.Manage); err != nil {
		return access.ToConnect(err)
	}
	target := &hostv1.PortServiceDialRequest{Data: first.GetData()}
	port, path := first.GetTcpPort(), first.GetUnixPath()
	switch {
	case port > 0 && port <= 65535 && path == "":
		target.Target = &hostv1.PortServiceDialRequest_TcpPort{TcpPort: port}
	case port == 0 && filepath.IsAbs(path) && !strings.ContainsRune(path, 0):
		target.Target = &hostv1.PortServiceDialRequest_UnixPath{UnixPath: path}
	default:
		return access.ToConnect(access.ErrInvalid)
	}
	client, release, err := p.hostLinks.Use(ctx, host)
	if err != nil {
		return access.ToConnect(err)
	}
	defer release()

	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	downstream := client.Ports.Dial(ctx)
	defer func() { _ = downstream.CloseResponse() }()
	if err := downstream.Send(target); err != nil && !errors.Is(err, io.EOF) {
		return fmt.Errorf("plugin dial: opening the host connection: %w", err)
	}
	sent := make(chan error, 1)
	go func() {
		err := copyPluginToHost(upstream, downstream)
		if err != nil {
			cancel()
		}
		sent <- err
	}()
	for {
		response, err := downstream.Receive()
		if errors.Is(err, io.EOF) {
			return nil
		}
		if err != nil {
			select {
			case sourceErr := <-sent:
				if sourceErr != nil {
					return sourceErr
				}
			default:
			}
			return err
		}
		if data := response.GetData(); len(data) != 0 {
			if err := upstream.Send(&pluginv1.ServerServiceDialNodeResponse{Data: data}); err != nil {
				return err
			}
		}
	}
}

func copyPluginToHost(
	upstream *connect.BidiStream[pluginv1.ServerServiceDialNodeRequest, pluginv1.ServerServiceDialNodeResponse],
	downstream *connect.BidiStreamForClient[hostv1.PortServiceDialRequest, hostv1.PortServiceDialResponse],
) error {
	for {
		message, err := upstream.Receive()
		if errors.Is(err, io.EOF) {
			return downstream.CloseRequest()
		}
		if err != nil {
			return err
		}
		if message.GetNode() != "" || message.GetTcpPort() != 0 || message.GetUnixPath() != "" {
			return access.ToConnect(access.ErrInvalid)
		}
		if err := downstream.Send(&hostv1.PortServiceDialRequest{Data: message.GetData()}); err != nil {
			if errors.Is(err, io.EOF) {
				return nil
			}
			return err
		}
	}
}
