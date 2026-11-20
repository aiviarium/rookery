package agent

import (
	"context"
	"errors"
	"fmt"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/cborx"
)

// DesktopProvider is the host service that the desktop's own session registers,
// with `box desktop serve`. DesktopService in the daemon relays to it, and the
// server reaches the desktop's RFB stream through PortService instead.
const DesktopProvider = "rookery/desktop"

// The desktop provider's methods. Its arguments and answers are CBOR of the
// structures below.
const (
	DesktopMethodScreenshot   = "screenshot"
	DesktopMethodInput        = "input"
	DesktopMethodClipboardGet = "clipboard.get"
	DesktopMethodClipboardSet = "clipboard.set"
	DesktopMethodPause        = "pause"
	DesktopMethodResume       = "resume"
	DesktopMethodState        = "state"
)

// DesktopMethods are the methods the provider registers.
var DesktopMethods = []string{
	DesktopMethodScreenshot,
	DesktopMethodInput,
	DesktopMethodClipboardGet,
	DesktopMethodClipboardSet,
	DesktopMethodPause,
	DesktopMethodResume,
	DesktopMethodState,
}

// DesktopScreenshotArgs is a screenshot's arguments: the size to scale to, zero
// for the screen's own.
type DesktopScreenshotArgs struct {
	Width  uint32 `cbor:"width"`
	Height uint32 `cbor:"height"`
}

// DesktopScreenshotResult is a screenshot: a PNG and the size it came out at.
type DesktopScreenshotResult struct {
	PNG    []byte `cbor:"png"`
	Width  uint32 `cbor:"width"`
	Height uint32 `cbor:"height"`
}

// The kinds of desktop input. A DesktopInput is a click, some text, a chord or a
// scroll, and only the fields of its kind are set.
const (
	DesktopInputClick  = "click"
	DesktopInputText   = "text"
	DesktopInputKey    = "key"
	DesktopInputScroll = "scroll"
)

// DesktopInput is one piece of input for the desktop.
type DesktopInput struct {
	Kind string `cbor:"kind"`

	// A click's pixel and button, and how many.
	X      int    `cbor:"x"`
	Y      int    `cbor:"y"`
	Button string `cbor:"button"`
	Count  int    `cbor:"count"`

	// Text to type, a chord to press, or the way to scroll.
	Text      string `cbor:"text"`
	Chord     string `cbor:"chord"`
	Direction string `cbor:"direction"`
}

// DesktopClipboard is the clipboard's text.
type DesktopClipboard struct {
	Text string `cbor:"text"`
}

// DesktopStateResult is the provider's answer about the desktop.
type DesktopStateResult struct {
	Paused  bool   `cbor:"paused"`
	Display string `cbor:"display"`
	User    string `cbor:"user"`
	Width   uint32 `cbor:"width"`
	Height  uint32 `cbor:"height"`
}

// desktopService serves DesktopService: the host's desktop, through its provider.
type desktopService struct {
	server *Server
}

// caller checks that a caller may use the desktop: the server's socket may, and
// a local program only as the desktop's own account.
func (h *desktopService) caller(ctx context.Context) (*caller, error) {
	caller, err := callerOf(ctx)
	if err != nil {
		return nil, err
	}
	if caller.role == roleUser && caller.account.Name != h.server.config.DesktopUser {
		return nil, denied("agent: the desktop belongs to %s, and this socket is %s",
			h.server.config.DesktopUser, caller.account.Name)
	}
	return caller, nil
}

// call passes one call to the desktop provider.
func (h *desktopService) call(ctx context.Context, method string, args, result any) error {
	encoded, err := cborx.Marshal(args)
	if err != nil {
		return err
	}
	answer, err := h.server.registry.call(ctx, DesktopProvider, method, encoded)
	if err != nil {
		if errors.Is(err, ErrUnavailable) {
			return fmt.Errorf("%w: no desktop provider is registered on this host", ErrUnavailable)
		}
		return err
	}
	if result == nil {
		return nil
	}
	if len(answer) == 0 {
		return nil
	}
	return cborx.Unmarshal(answer, result)
}

func (h *desktopService) Screenshot(ctx context.Context, request *connect.Request[hostv1.DesktopServiceScreenshotRequest]) (*connect.Response[hostv1.DesktopServiceScreenshotResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}

	result := &DesktopScreenshotResult{}
	err := h.call(ctx, DesktopMethodScreenshot, &DesktopScreenshotArgs{
		Width:  request.Msg.GetWidth(),
		Height: request.Msg.GetHeight(),
	}, result)
	if err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceScreenshotResponse{
		Png:    result.PNG,
		Width:  result.Width,
		Height: result.Height,
	}), nil
}

func (h *desktopService) Input(ctx context.Context, request *connect.Request[hostv1.DesktopServiceInputRequest]) (*connect.Response[hostv1.DesktopServiceInputResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}

	input, err := desktopInput(request.Msg)
	if err != nil {
		return nil, err
	}
	if err := h.call(ctx, DesktopMethodInput, input, nil); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceInputResponse{}), nil
}

// desktopInput is the provider's shape of an input call.
func desktopInput(request *hostv1.DesktopServiceInputRequest) (*DesktopInput, error) {
	switch input := request.GetInput().(type) {
	case *hostv1.DesktopServiceInputRequest_Click:
		button := "left"
		switch input.Click.GetButton() {
		case hostv1.MouseButton_MOUSE_BUTTON_MIDDLE:
			button = "middle"
		case hostv1.MouseButton_MOUSE_BUTTON_RIGHT:
			button = "right"
		}
		return &DesktopInput{
			Kind:   DesktopInputClick,
			X:      int(input.Click.GetX()),
			Y:      int(input.Click.GetY()),
			Button: button,
			Count:  int(input.Click.GetCount()),
		}, nil
	case *hostv1.DesktopServiceInputRequest_Text:
		return &DesktopInput{Kind: DesktopInputText, Text: input.Text.GetText()}, nil
	case *hostv1.DesktopServiceInputRequest_Key:
		return &DesktopInput{Kind: DesktopInputKey, Chord: input.Key.GetChord()}, nil
	case *hostv1.DesktopServiceInputRequest_Scroll:
		direction := "up"
		if input.Scroll.GetDirection() == hostv1.ScrollDirection_SCROLL_DIRECTION_DOWN {
			direction = "down"
		}
		return &DesktopInput{
			Kind:      DesktopInputScroll,
			Direction: direction,
			Count:     int(input.Scroll.GetCount()),
		}, nil
	default:
		return nil, invalid("agent: an input call says what to do: a click, text, a key or a scroll")
	}
}

func (h *desktopService) GetClipboard(ctx context.Context, _ *connect.Request[hostv1.DesktopServiceGetClipboardRequest]) (*connect.Response[hostv1.DesktopServiceGetClipboardResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}

	result := &DesktopClipboard{}
	if err := h.call(ctx, DesktopMethodClipboardGet, struct{}{}, result); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceGetClipboardResponse{Text: result.Text}), nil
}

func (h *desktopService) SetClipboard(ctx context.Context, request *connect.Request[hostv1.DesktopServiceSetClipboardRequest]) (*connect.Response[hostv1.DesktopServiceSetClipboardResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}

	if err := h.call(ctx, DesktopMethodClipboardSet, &DesktopClipboard{Text: request.Msg.GetText()}, nil); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceSetClipboardResponse{}), nil
}

func (h *desktopService) Pause(ctx context.Context, _ *connect.Request[hostv1.DesktopServicePauseRequest]) (*connect.Response[hostv1.DesktopServicePauseResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}
	if err := h.call(ctx, DesktopMethodPause, struct{}{}, nil); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServicePauseResponse{}), nil
}

func (h *desktopService) Resume(ctx context.Context, _ *connect.Request[hostv1.DesktopServiceResumeRequest]) (*connect.Response[hostv1.DesktopServiceResumeResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}
	if err := h.call(ctx, DesktopMethodResume, struct{}{}, nil); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceResumeResponse{}), nil
}

// State says whether a desktop answers. A host with no provider answers that it
// has none rather than failing: a screen with no desktop is a state, not an
// error.
func (h *desktopService) State(ctx context.Context, _ *connect.Request[hostv1.DesktopServiceStateRequest]) (*connect.Response[hostv1.DesktopServiceStateResponse], error) {
	if _, err := h.caller(ctx); err != nil {
		return nil, err
	}

	if !h.server.registry.has(DesktopProvider) {
		return connect.NewResponse(&hostv1.DesktopServiceStateResponse{Desktop: &hostv1.DesktopState{
			Available: false,
			User:      h.server.config.DesktopUser,
			Reason:    "no desktop provider is registered on this host",
		}}), nil
	}

	result := &DesktopStateResult{}
	if err := h.call(ctx, DesktopMethodState, struct{}{}, result); err != nil {
		return nil, toConnect(err)
	}
	return connect.NewResponse(&hostv1.DesktopServiceStateResponse{Desktop: &hostv1.DesktopState{
		Available: true,
		Paused:    result.Paused,
		Display:   result.Display,
		User:      result.User,
		Width:     result.Width,
		Height:    result.Height,
	}}), nil
}
