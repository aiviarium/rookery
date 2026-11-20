package boxcli

import (
	"bytes"
	"context"
	"fmt"
	"image/png"
	"os"
	"os/user"
	"sync"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/x11"
)

// runDesktopServe registers the desktop provider: the host service that
// rookeryd's DesktopService relays screenshots, input and the clipboard to.
//
// The desktop role's systemd user unit runs it inside the desktop session, with
// the desktop environment in its environment.
func runDesktopServe(ctx context.Context, options Options, args []string) error {
	display := ""
	rest := make([]string, 0, len(args))
	for i := 0; i < len(args); i++ {
		switch {
		case args[i] == "--display" && i+1 < len(args):
			display = args[i+1]
			i++
		case len(args[i]) > len("--display=") && args[i][:len("--display=")] == "--display=":
			display = args[i][len("--display="):]
		default:
			rest = append(rest, args[i])
		}
	}
	if len(rest) != 0 {
		return usage("usage: box desktop serve [--display :N]")
	}
	if display == "" {
		display = os.Getenv("DISPLAY")
	}
	if display == "" {
		return usage("box desktop serve: there is no DISPLAY, and no --display")
	}

	conn, err := x11.Open(ctx, display)
	if err != nil {
		return fmt.Errorf("box: opening the desktop: %w", err)
	}
	defer func() { _ = conn.Close() }()

	provider := &desktopProvider{conn: conn, user: accountName()}
	service := &hostv1.HostService{
		Name:        agent.DesktopProvider,
		Methods:     agent.DesktopMethods(),
		Description: "the desktop of this host, served by box",
	}
	options.logf("box: the desktop on %s is serving", display)
	return serveHostService(ctx, options, service, provider.handle)
}

// accountName is the account running box, which is the desktop's.
func accountName() string {
	if current, err := user.Current(); err == nil {
		return current.Username
	}
	return os.Getenv("USER")
}

// desktopProvider is the desktop's half of the provider: it takes screenshots,
// puts input in, and owns the clipboard.
//
// Its calls run at once, because pause and resume arrive as calls of their own
// while a screenshot waits. Input itself is serialised by its connection, so two
// clicks cannot interleave halfway through.
type desktopProvider struct {
	conn *x11.Conn
	user string

	paused   bool
	resumed  chan struct{}
	pauseMux sync.Mutex
}

// handle runs one provider call.
func (p *desktopProvider) handle(ctx context.Context, method string, args []byte) ([]byte, error) {
	switch method {
	case agent.DesktopMethodScreenshot:
		var request agent.DesktopScreenshotArgs
		if err := decodeArgs(args, &request); err != nil {
			return nil, err
		}
		if err := p.wait(ctx); err != nil {
			return nil, err
		}

		picture, err := p.conn.Screenshot(ctx)
		if err != nil {
			return nil, err
		}
		picture = x11.Scale(picture, int(request.Width), int(request.Height))

		var encoded bytes.Buffer
		if err := png.Encode(&encoded, picture); err != nil {
			return nil, fmt.Errorf("box: encoding the screenshot: %w", err)
		}
		bounds := picture.Bounds()
		return cborx.Marshal(&agent.DesktopScreenshotResult{
			PNG:    encoded.Bytes(),
			Width:  uint32(bounds.Dx()), //nolint:gosec // a screen is not negative.
			Height: uint32(bounds.Dy()), //nolint:gosec // a screen is not negative.
		})

	case agent.DesktopMethodInput:
		var input agent.DesktopInput
		if err := decodeArgs(args, &input); err != nil {
			return nil, err
		}
		if err := p.wait(ctx); err != nil {
			return nil, err
		}
		return nil, p.input(ctx, input)

	case agent.DesktopMethodClipboardGet:
		if err := p.wait(ctx); err != nil {
			return nil, err
		}
		text, err := p.conn.GetClipboard(ctx)
		if err != nil {
			return nil, err
		}
		return cborx.Marshal(&agent.DesktopClipboard{Text: text})

	case agent.DesktopMethodClipboardSet:
		var clipboard agent.DesktopClipboard
		if err := decodeArgs(args, &clipboard); err != nil {
			return nil, err
		}
		if err := p.wait(ctx); err != nil {
			return nil, err
		}
		if err := p.conn.SetClipboard(ctx, clipboard.Text); err != nil {
			return nil, err
		}
		return nil, nil

	case agent.DesktopMethodPause:
		p.pause()
		return nil, nil

	case agent.DesktopMethodResume:
		p.resume()
		return nil, nil

	case agent.DesktopMethodState:
		width, height := p.conn.Size()
		return cborx.Marshal(&agent.DesktopStateResult{
			Paused:  p.isPaused(),
			Display: p.conn.Display(),
			User:    p.user,
			Width:   uint32(width),  //nolint:gosec // a screen is not negative.
			Height:  uint32(height), //nolint:gosec // a screen is not negative.
		})

	default:
		return nil, fmt.Errorf("%w: the desktop has no method %s", agent.ErrInvalid, method)
	}
}

// input puts one click, some text, a chord or a scroll into the desktop.
func (p *desktopProvider) input(ctx context.Context, input agent.DesktopInput) error {
	switch input.Kind {
	case agent.DesktopInputClick:
		button := x11.ButtonLeft
		switch input.Button {
		case "middle":
			button = x11.ButtonMiddle
		case "right":
			button = x11.ButtonRight
		}
		return p.conn.Click(ctx, input.X, input.Y, button, input.Count)
	case agent.DesktopInputText:
		return p.conn.Type(ctx, input.Text)
	case agent.DesktopInputKey:
		return p.conn.Key(ctx, input.Chord)
	case agent.DesktopInputScroll:
		direction := x11.Up
		if input.Direction == "down" {
			direction = x11.Down
		}
		return p.conn.Scroll(ctx, direction, input.Count)
	default:
		return fmt.Errorf("%w: the desktop input %q is not a click, text, a key or a scroll",
			agent.ErrInvalid, input.Kind)
	}
}

// decodeArgs reads a call's arguments, of which there may be none.
func decodeArgs(args []byte, into any) error {
	if len(args) == 0 {
		return nil
	}
	if err := cborx.Unmarshal(args, into); err != nil {
		return fmt.Errorf("%w: the desktop's arguments: %w", agent.ErrInvalid, err)
	}
	return nil
}

// wait holds callers while the desktop is paused, until Resume or the call's end.
func (p *desktopProvider) wait(ctx context.Context) error {
	p.pauseMux.Lock()
	for p.paused {
		resumed := p.resumed
		p.pauseMux.Unlock()
		select {
		case <-resumed:
		case <-ctx.Done():
			return ctx.Err()
		}
		p.pauseMux.Lock()
	}
	p.pauseMux.Unlock()
	return nil
}

func (p *desktopProvider) pause() {
	p.pauseMux.Lock()
	defer p.pauseMux.Unlock()
	if p.paused {
		return
	}
	p.paused = true
	p.resumed = make(chan struct{})
}

func (p *desktopProvider) resume() {
	p.pauseMux.Lock()
	defer p.pauseMux.Unlock()
	if !p.paused {
		return
	}
	p.paused = false
	close(p.resumed)
}

func (p *desktopProvider) isPaused() bool {
	p.pauseMux.Lock()
	defer p.pauseMux.Unlock()
	return p.paused
}
