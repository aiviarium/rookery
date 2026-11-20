// Package desktop exposes a workstation's shared desktop through the edge.
package desktop

import (
	"context"
	"fmt"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/hosts"
	"github.com/aiviarium/rookery/internal/link"
)

type hostStore interface {
	Get(context.Context, string) (hosts.Record, error)
}

// Service relays desktop controls to the daemon and opens RFB streams.
type Service struct {
	hosts   hostStore
	links   *link.Manager
	checker *access.Checker
}

// New returns the desktop service for the host store and shared links.
func New(hosts hostStore, links *link.Manager, checker *access.Checker) *Service {
	return &Service{hosts: hosts, links: links, checker: checker}
}

func (s *Service) checkHost(ctx context.Context, name string) error {
	if _, err := access.ParseResource(access.HostURI(name)); err != nil {
		return err
	}
	host, err := s.hosts.Get(ctx, name)
	if err != nil {
		return err
	}
	return hosts.Check(host, hosts.CapabilityDesktop)
}

func (s *Service) use(ctx context.Context, name string) (*link.Client, func(), error) {
	if err := s.checkHost(ctx, name); err != nil {
		return nil, nil, access.ToConnect(err)
	}
	client, release, err := s.links.Use(ctx, name)
	return client, release, access.ToConnect(err)
}

// Screenshot returns the screen as a PNG at the requested size.
func (s *Service) Screenshot(ctx context.Context, req *connect.Request[rookeryv1.DesktopServiceScreenshotRequest]) (*connect.Response[rookeryv1.DesktopServiceScreenshotResponse], error) {
	client, release, err := s.use(ctx, req.Msg.GetHost())
	if err != nil {
		return nil, err
	}
	defer release()
	response, err := client.Desktop.Screenshot(ctx, connect.NewRequest(&hostv1.DesktopServiceScreenshotRequest{
		Width: req.Msg.GetWidth(), Height: req.Msg.GetHeight(),
	}))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.DesktopServiceScreenshotResponse{
		Png: response.Msg.GetPng(), Width: response.Msg.GetWidth(), Height: response.Msg.GetHeight(),
	}), nil
}

// Input sends one input event to the workstation.
func (s *Service) Input(ctx context.Context, req *connect.Request[rookeryv1.DesktopServiceInputRequest]) (*connect.Response[rookeryv1.DesktopServiceInputResponse], error) {
	input := &hostv1.DesktopServiceInputRequest{}
	switch event := req.Msg.GetInput().(type) {
	case *rookeryv1.DesktopServiceInputRequest_Click:
		click := event.Click
		input.Input = &hostv1.DesktopServiceInputRequest_Click{Click: &hostv1.DesktopClick{
			X: click.GetX(), Y: click.GetY(), Button: hostv1.MouseButton(click.GetButton()), Count: click.GetCount(),
		}}
	case *rookeryv1.DesktopServiceInputRequest_Text:
		input.Input = &hostv1.DesktopServiceInputRequest_Text{Text: &hostv1.DesktopText{Text: event.Text.GetText()}}
	case *rookeryv1.DesktopServiceInputRequest_Key:
		input.Input = &hostv1.DesktopServiceInputRequest_Key{Key: &hostv1.DesktopKey{Chord: event.Key.GetChord()}}
	case *rookeryv1.DesktopServiceInputRequest_Scroll:
		input.Input = &hostv1.DesktopServiceInputRequest_Scroll{Scroll: &hostv1.DesktopScroll{
			Direction: hostv1.ScrollDirection(event.Scroll.GetDirection()), Count: event.Scroll.GetCount(),
		}}
	default:
		return nil, access.ToConnect(fmt.Errorf("%w: desktop input requires a click, text, key or scroll", access.ErrInvalid))
	}
	client, release, err := s.use(ctx, req.Msg.GetHost())
	if err != nil {
		return nil, err
	}
	defer release()
	if _, err := client.Desktop.Input(ctx, connect.NewRequest(input)); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.DesktopServiceInputResponse{}), nil
}

// GetClipboard reads the workstation's clipboard text.
func (s *Service) GetClipboard(ctx context.Context, req *connect.Request[rookeryv1.DesktopServiceGetClipboardRequest]) (*connect.Response[rookeryv1.DesktopServiceGetClipboardResponse], error) {
	client, release, err := s.use(ctx, req.Msg.GetHost())
	if err != nil {
		return nil, err
	}
	defer release()
	response, err := client.Desktop.GetClipboard(ctx, connect.NewRequest(&hostv1.DesktopServiceGetClipboardRequest{}))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.DesktopServiceGetClipboardResponse{Text: response.Msg.GetText()}), nil
}

// SetClipboard sets the workstation's clipboard text.
func (s *Service) SetClipboard(ctx context.Context, req *connect.Request[rookeryv1.DesktopServiceSetClipboardRequest]) (*connect.Response[rookeryv1.DesktopServiceSetClipboardResponse], error) {
	client, release, err := s.use(ctx, req.Msg.GetHost())
	if err != nil {
		return nil, err
	}
	defer release()
	_, err = client.Desktop.SetClipboard(ctx, connect.NewRequest(&hostv1.DesktopServiceSetClipboardRequest{Text: req.Msg.GetText()}))
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.DesktopServiceSetClipboardResponse{}), nil
}

// Pause holds cooperative desktop calls until Resume.
func (s *Service) Pause(ctx context.Context, req *connect.Request[rookeryv1.DesktopServicePauseRequest]) (*connect.Response[rookeryv1.DesktopServicePauseResponse], error) {
	client, release, err := s.use(ctx, req.Msg.GetHost())
	if err != nil {
		return nil, err
	}
	defer release()
	if _, err := client.Desktop.Pause(ctx, connect.NewRequest(&hostv1.DesktopServicePauseRequest{})); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.DesktopServicePauseResponse{}), nil
}

// Resume releases paused desktop calls.
func (s *Service) Resume(ctx context.Context, req *connect.Request[rookeryv1.DesktopServiceResumeRequest]) (*connect.Response[rookeryv1.DesktopServiceResumeResponse], error) {
	client, release, err := s.use(ctx, req.Msg.GetHost())
	if err != nil {
		return nil, err
	}
	defer release()
	if _, err := client.Desktop.Resume(ctx, connect.NewRequest(&hostv1.DesktopServiceResumeRequest{})); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.DesktopServiceResumeResponse{}), nil
}
