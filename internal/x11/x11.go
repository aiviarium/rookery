// Package x11 screenshots a desktop and puts input into it over the X11
// protocol itself, with no cgo and no Xlib: GetImage for the pixels, the XTest
// extension for pointer and keyboard input, and the CLIPBOARD selection for
// text.
//
// A Conn is one X connection and is safe for concurrent use; its methods take a
// lock, so input arrives in the order calls are made. The selection lives on a
// second connection of its own, because the client that owns a selection has to
// sit in an event loop answering other clients' requests while its caller keeps
// taking screenshots.
//
// The display is named as X names it, ":1" or "host:0"; an empty one is $DISPLAY.
// The authority comes from XAUTHORITY, as it does for every X client.
package x11

import (
	"context"
	"errors"
	"fmt"
	"image"
	"sync"

	"github.com/jezek/xgb"
	"github.com/jezek/xgb/xproto"
	"github.com/jezek/xgb/xtest"
)

// Conn is a connection to an X display.
type Conn struct {
	display string
	conn    *xgb.Conn
	root    xproto.Window
	inputMu sync.Mutex

	// mu serialises the connection, which xgb does not do for its callers.
	mu sync.Mutex

	// The keyboard is read once, the first time something is typed.
	layoutOnce sync.Once
	layout     *layout
	layoutErr  error

	clipboardMu sync.Mutex
	clipboard   *clipboard
	closed      bool
}

// Open connects to a display and checks that it has the XTest extension, without
// which there is no input.
func Open(ctx context.Context, display string) (*Conn, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}

	conn, err := xgb.NewConnDisplay(display)
	if err != nil {
		return nil, fmt.Errorf("x11: connecting to %q: %w", display, err)
	}
	if err := xtest.Init(conn); err != nil {
		conn.Close()
		return nil, fmt.Errorf("x11: %q has no XTest extension, so nothing can type into it: %w", display, err)
	}

	setup := xproto.Setup(conn)
	screen := setup.DefaultScreen(conn)
	return &Conn{
		display: display,
		conn:    conn,
		root:    screen.Root,
	}, nil
}

// Close ends the connection and stops owning the clipboard.
func (c *Conn) Close() error {
	c.inputMu.Lock()
	defer c.inputMu.Unlock()
	c.clipboardMu.Lock()
	defer c.clipboardMu.Unlock()
	if c.closed {
		return nil
	}
	if c.clipboard != nil {
		c.clipboard.close()
	}
	c.closed = true
	c.conn.Close()
	return nil
}

// Display is the display's name.
func (c *Conn) Display() string { return c.display }

// Size reads the root window's current geometry.
func (c *Conn) Size(ctx context.Context) (width, height uint16, err error) {
	if err := ctx.Err(); err != nil {
		return 0, 0, err
	}
	c.mu.Lock()
	reply, err := xproto.GetGeometry(c.conn, xproto.Drawable(c.root)).Reply()
	c.mu.Unlock()
	if err != nil {
		return 0, 0, fmt.Errorf("x11: reading the screen geometry: %w", err)
	}
	if reply == nil {
		return 0, 0, fmt.Errorf("x11: reading the screen geometry: %w", errNoReply)
	}
	return reply.Width, reply.Height, nil
}

// Screenshot reads the whole screen.
func (c *Conn) Screenshot(ctx context.Context) (image.Image, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	width, height, err := c.Size(ctx)
	if err != nil {
		return nil, err
	}

	c.mu.Lock()
	reply, err := xproto.GetImage(c.conn, xproto.ImageFormatZPixmap, xproto.Drawable(c.root), 0, 0, width, height, ^uint32(0)).Reply()
	c.mu.Unlock()
	if err != nil {
		return nil, fmt.Errorf("x11: taking a screenshot: %w", err)
	}
	if reply == nil {
		return nil, fmt.Errorf("x11: taking a screenshot: %w", errNoReply)
	}
	return decodeZPixmap(reply.Data, int(width), int(height))
}

// errNoReply is an X request that the server answered with nothing at all.
var errNoReply = errors.New("the server sent no reply")

// decodeZPixmap turns a ZPixmap of a 24- or 32-bit screen into an image: four
// bytes a pixel, blue, green, red, unused, as every X server Rookery meets
// stores them.
func decodeZPixmap(data []byte, width, height int) (image.Image, error) {
	if len(data) < width*height*4 {
		return nil, fmt.Errorf("x11: a %dx%d screen takes %d bytes and the server sent %d", width, height, width*height*4, len(data))
	}

	picture := image.NewNRGBA(image.Rect(0, 0, width, height))
	for y := 0; y < height; y++ {
		row := data[y*width*4:]
		for x := 0; x < width; x++ {
			pixel := row[x*4:]
			offset := picture.PixOffset(x, y)
			picture.Pix[offset+0] = pixel[2]
			picture.Pix[offset+1] = pixel[1]
			picture.Pix[offset+2] = pixel[0]
			picture.Pix[offset+3] = 0xff
		}
	}
	return picture, nil
}

// Scale returns an image scaled to a size, keeping its proportions when only one
// of the sides is given, and leaving it alone when both are zero. It is nearest
// neighbour on purpose: a screenshot for a thumbnail is not a photograph, and
// this keeps a two-pixel cursor visible.
func Scale(source image.Image, width, height uint32) (image.Image, error) {
	bounds := source.Bounds()
	sourceWidth, sourceHeight := uint64(bounds.Dx()), uint64(bounds.Dy()) //nolint:gosec // a screen's dimensions are positive.
	if width == 0 && height == 0 {
		return source, nil
	}
	w, h := uint64(width), uint64(height)
	switch {
	case sourceWidth == 0 || sourceHeight == 0:
		return nil, errors.New("x11: cannot scale an empty screenshot")
	case w == 0:
		w = max(1, sourceWidth*h/sourceHeight)
	case h == 0:
		h = max(1, sourceHeight*w/sourceWidth)
	}
	if w == sourceWidth && h == sourceHeight {
		return source, nil
	}
	const maxPixels = 32 << 20
	if w > maxPixels/h {
		return nil, fmt.Errorf("x11: the requested screenshot is too large (%dx%d)", w, h)
	}

	targetWidth, targetHeight := int(w), int(h) //nolint:gosec // both dimensions fit within the pixel limit.
	scaled := image.NewNRGBA(image.Rect(0, 0, targetWidth, targetHeight))
	for y := 0; y < targetHeight; y++ {
		sourceY := bounds.Min.Y + int(uint64(y)*sourceHeight/h) //nolint:gosec // the offset is within the source image.
		for x := 0; x < targetWidth; x++ {
			sourceX := bounds.Min.X + int(uint64(x)*sourceWidth/w) //nolint:gosec // the offset is within the source image.
			scaled.Set(x, y, source.At(sourceX, sourceY))
		}
	}
	return scaled, nil
}
