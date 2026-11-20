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
	width   uint16
	height  uint16
	depth   byte

	// mu serialises the connection, which xgb does not do for its callers.
	mu sync.Mutex

	// The keyboard is read once, the first time something is typed.
	layoutOnce sync.Once
	layout     *layout
	layoutErr  error

	clipboardOnce sync.Once
	clipboard     *clipboard
	clipboardErr  error
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
		width:   screen.WidthInPixels,
		height:  screen.HeightInPixels,
		depth:   screen.RootDepth,
	}, nil
}

// Close ends the connection and stops owning the clipboard.
func (c *Conn) Close() error {
	if c.clipboard != nil {
		c.clipboard.close()
		c.clipboard = nil
	}
	c.conn.Close()
	return nil
}

// Display is the display's name.
func (c *Conn) Display() string { return c.display }

// Size is the screen's size in pixels.
func (c *Conn) Size() (width, height int) { return int(c.width), int(c.height) }

// Screenshot reads the whole screen.
func (c *Conn) Screenshot(ctx context.Context) (image.Image, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	width, height := c.Size()

	c.mu.Lock()
	reply, err := xproto.GetImage(c.conn, xproto.ImageFormatZPixmap, xproto.Drawable(c.root), 0, 0, c.width, c.height, ^uint32(0)).Reply()
	c.mu.Unlock()
	if err != nil {
		return nil, fmt.Errorf("x11: taking a screenshot: %w", err)
	}
	if reply == nil {
		return nil, fmt.Errorf("x11: taking a screenshot: %w", errNoReply)
	}
	return decodeZPixmap(reply.Data, width, height)
}

// errNoReply is an X request that the server answered with nothing at all.
var errNoReply = fmt.Errorf("the server sent no reply")

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
func Scale(source image.Image, width, height int) image.Image {
	bounds := source.Bounds()
	sourceWidth, sourceHeight := bounds.Dx(), bounds.Dy()
	if width <= 0 && height <= 0 {
		return source
	}
	if width <= 0 {
		width = sourceWidth * height / sourceHeight
	}
	if height <= 0 {
		height = sourceHeight * width / sourceWidth
	}
	if width == sourceWidth && height == sourceHeight {
		return source
	}
	if width < 1 {
		width = 1
	}
	if height < 1 {
		height = 1
	}

	scaled := image.NewNRGBA(image.Rect(0, 0, width, height))
	for y := 0; y < height; y++ {
		sourceY := bounds.Min.Y + y*sourceHeight/height
		for x := 0; x < width; x++ {
			sourceX := bounds.Min.X + x*sourceWidth/width
			scaled.Set(x, y, source.At(sourceX, sourceY))
		}
	}
	return scaled
}
