package server

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"strconv"

	"connectrpc.com/connect"
	"golang.org/x/sync/errgroup"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/tailnet"
)

type tailnetService struct {
	manager *tailnet.Manager
	hosts   *hosts.Hosts
}

func (s tailnetService) Status(_ context.Context, _ *connect.Request[rookeryv1.TailnetServiceStatusRequest]) (*connect.Response[rookeryv1.TailnetServiceStatusResponse], error) {
	return connect.NewResponse(s.manager.Status()), nil
}

func (s tailnetService) Configure(ctx context.Context, request *connect.Request[rookeryv1.TailnetServiceConfigureRequest]) (*connect.Response[rookeryv1.TailnetServiceConfigureResponse], error) {
	if err := s.manager.Configure(ctx, request.Msg.GetControlUrl(), request.Msg.GetTag(), request.Msg.GetApiKey()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.TailnetServiceConfigureResponse{}), nil
}

func (s tailnetService) ListDevices(ctx context.Context, _ *connect.Request[rookeryv1.TailnetServiceListDevicesRequest]) (*connect.Response[rookeryv1.TailnetServiceListDevicesResponse], error) {
	devices, err := s.manager.Devices(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	records, err := s.hosts.List(ctx, hosts.Filter{})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	known := make(map[string]struct{}, 2*len(records))
	for _, record := range records {
		known[record.Name] = struct{}{}
		if record.Address != "" {
			known[record.Address] = struct{}{}
		}
	}
	response := &rookeryv1.TailnetServiceListDevicesResponse{}
	for _, device := range devices {
		_, found := known[device.Hostname]
		for _, address := range device.Addresses {
			if _, knownAddress := known[address]; knownAddress {
				found = true
				break
			}
		}
		if found {
			continue
		}
		item := &rookeryv1.TailnetDevice{
			Id: device.ID, Hostname: device.Hostname, Addresses: device.Addresses, Tags: device.Tags, Online: device.Online,
		}
		if !device.LastSeen.IsZero() {
			item.LastSeen = timestamppb.New(device.LastSeen)
		}
		response.Devices = append(response.Devices, item)
	}
	return connect.NewResponse(response), nil
}

type localRequestBody struct{}

func withLocalRequestBody(next http.Handler) http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		ctx := context.WithValue(r.Context(), localRequestBody{}, r.Body)
		next.ServeHTTP(w, r.WithContext(ctx))
	})
}

// Dial opens a local admin's byte stream through the tailnet.
func (l *LocalService) Dial(ctx context.Context, stream *connect.BidiStream[rookeryv1.LocalServiceDialRequest, rookeryv1.LocalServiceDialResponse]) error {
	if l.tailnet == nil {
		return access.ToConnect(access.ErrUnavailable)
	}
	opening, err := stream.Receive()
	if errors.Is(err, io.EOF) {
		return access.ToConnect(fmt.Errorf("%w: a dial stream starts with its destination", access.ErrInvalid))
	}
	if err != nil {
		return err
	}
	host, port := opening.GetHost(), opening.GetPort()
	if host == "" || port == 0 || port > 65535 || len(opening.GetData()) > 0 {
		return access.ToConnect(fmt.Errorf("%w: a dial stream starts with a host and port", access.ErrInvalid))
	}
	connection, err := l.tailnet.Dial(ctx, "tcp", net.JoinHostPort(host, strconv.FormatUint(uint64(port), 10)))
	if err != nil {
		return access.ToConnect(fmt.Errorf("%w: could not reach the tailnet destination", access.ErrUnavailable))
	}
	defer func() { _ = connection.Close() }()

	ctx, stop := context.WithCancel(ctx)
	defer stop()
	group, groupCtx := errgroup.WithContext(ctx)
	cancelIO := context.AfterFunc(groupCtx, func() {
		_ = connection.Close()
		if body, ok := ctx.Value(localRequestBody{}).(io.Closer); ok {
			_ = body.Close()
		}
	})
	defer cancelIO()
	group.Go(func() error {
		defer stop()
		buffer := make([]byte, 64<<10)
		for {
			n, err := connection.Read(buffer)
			if n != 0 {
				if err := stream.Send(&rookeryv1.LocalServiceDialResponse{Data: buffer[:n]}); err != nil {
					return err
				}
			}
			if errors.Is(err, io.EOF) || groupCtx.Err() != nil {
				return nil
			}
			if err != nil {
				return fmt.Errorf("tailnet: reading from the destination: %w", err)
			}
		}
	})
	group.Go(func() error {
		for {
			packet, err := stream.Receive()
			if groupCtx.Err() != nil {
				return nil
			}
			if errors.Is(err, io.EOF) {
				if half, ok := connection.(interface{ CloseWrite() error }); ok {
					return half.CloseWrite()
				}
				return nil
			}
			if err != nil {
				return err
			}
			if packet.GetHost() != "" || packet.GetPort() != 0 {
				return fmt.Errorf("%w: a dial stream has one destination", access.ErrInvalid)
			}
			if _, err := connection.Write(packet.GetData()); err != nil {
				if groupCtx.Err() != nil {
					return nil
				}
				return fmt.Errorf("tailnet: writing to the destination: %w", err)
			}
		}
	})
	return access.ToConnect(group.Wait())
}

func (s *Server) runLeaderLoops(ctx context.Context, wired *handlers) error {
	group, ctx := errgroup.WithContext(ctx)
	group.Go(func() error { return wired.start(ctx) })
	if s.config.TailnetListen != "" {
		group.Go(func() error {
			select {
			case <-ctx.Done():
				return nil
			case <-wired.tailnet.Ready():
			}
			listener, err := wired.tailnet.Listen("tcp", s.config.TailnetListen)
			if err != nil {
				return err
			}
			server, err := s.newHTTPServer(listener, s.config.TLSEnabled(), s.edgeServer())
			if err != nil {
				_ = listener.Close()
				return err
			}
			<-ctx.Done()
			s.stop(ctx, []*http.Server{server})
			return nil
		})
	}
	return group.Wait()
}
