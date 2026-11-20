package x11

import (
	"context"
	"errors"
	"fmt"
	"sync"

	"github.com/jezek/xgb"
	"github.com/jezek/xgb/xproto"
)

// errClipboardClosed is a selection call on a closed connection.
var errClipboardClosed = errors.New("x11: the clipboard's connection is closed")

// clipboard owns and reads the CLIPBOARD selection.
//
// A client that owns a selection must answer the other clients that ask for it,
// so the selection has a connection of its own and a loop that does nothing else:
// the loop reads events and the calls made through the Conn, and the calls that
// take a screenshot or type keep going on the first connection meanwhile.
type clipboard struct {
	conn *xgb.Conn
	win  xproto.Window

	clipboardAtom xproto.Atom
	targetsAtom   xproto.Atom
	utf8Atom      xproto.Atom
	textAtom      xproto.Atom
	stringAtom    xproto.Atom
	atomAtom      xproto.Atom
	propertyAtom  xproto.Atom

	requests   chan any
	events     chan xgb.Event
	readerDone chan struct{}
	stop       chan struct{}
	done       chan struct{}
	stopOnce   sync.Once

	// Only the loop touches these.
	text    string
	owner   bool
	pending []chan getResult
}

type getResult struct {
	text string
	err  error
}

type getRequest struct{ result chan getResult }

type setRequest struct {
	text   string
	result chan error
}

// newClipboard opens a connection for the selection and creates its window.
func newClipboard(ctx context.Context, display string) (*clipboard, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	conn, err := xgb.NewConnDisplay(display)
	if err != nil {
		return nil, fmt.Errorf("x11: connecting for the clipboard: %w", err)
	}

	setup := xproto.Setup(conn)
	screen := setup.DefaultScreen(conn)

	clipboard := &clipboard{
		conn:       conn,
		requests:   make(chan any),
		events:     make(chan xgb.Event, 16),
		readerDone: make(chan struct{}),
		stop:       make(chan struct{}),
		done:       make(chan struct{}),
	}

	atoms := []struct {
		name string
		into *xproto.Atom
	}{
		{"CLIPBOARD", &clipboard.clipboardAtom},
		{"TARGETS", &clipboard.targetsAtom},
		{"UTF8_STRING", &clipboard.utf8Atom},
		{"TEXT", &clipboard.textAtom},
		{"STRING", &clipboard.stringAtom},
		{"ATOM", &clipboard.atomAtom},
		{"ROOKERY_CLIPBOARD", &clipboard.propertyAtom},
	}
	for _, atom := range atoms {
		reply, err := xproto.InternAtom(conn, false, uint16(len(atom.name)), atom.name).Reply() //nolint:gosec // an atom's name is short.
		if err != nil || reply == nil {
			conn.Close()
			if err == nil {
				err = errNoReply
			}
			return nil, fmt.Errorf("x11: asking for the atom %s: %w", atom.name, err)
		}
		*atom.into = reply.Atom
	}

	windowID, err := xproto.NewWindowId(conn)
	if err != nil {
		conn.Close()
		return nil, fmt.Errorf("x11: making the clipboard's window: %w", err)
	}
	clipboard.win = windowID
	// One pixel, outside the screen, never mapped: a selection needs a window to
	// belong to and nothing more.
	if err := xproto.CreateWindowChecked(conn, 0, windowID, screen.Root,
		-1, -1, 1, 1, 0, xproto.WindowClassInputOutput, 0, 0, nil).Check(); err != nil {
		conn.Close()
		return nil, fmt.Errorf("x11: creating the clipboard's window: %w", err)
	}

	go clipboard.read()
	go clipboard.run()
	return clipboard, nil
}

// read passes selection events to the loop until the X connection closes.
func (c *clipboard) read() {
	defer close(c.readerDone)
	for {
		event, err := c.conn.WaitForEvent()
		if err != nil || event == nil {
			return
		}
		select {
		case c.events <- event:
		case <-c.stop:
			return
		}
	}
}

// run is the selection's one loop.
func (c *clipboard) run() {
	defer close(c.done)
	defer func() {
		xproto.DestroyWindow(c.conn, c.win)
		c.conn.Close()
		<-c.readerDone
	}()

	for {
		select {
		case <-c.stop:
			return
		case request := <-c.requests:
			c.handleRequest(request)
		case event := <-c.events:
			c.handleEvent(event)
		case <-c.readerDone:
			return
		}
	}
}

// handleRequest runs one call on the selection.
func (c *clipboard) handleRequest(request any) {
	switch typed := request.(type) {
	case getRequest:
		if c.owner {
			typed.result <- getResult{text: c.text}
			return
		}
		c.pending = append(c.pending, typed.result)
		if len(c.pending) == 1 {
			xproto.ConvertSelection(c.conn, c.win, c.clipboardAtom, c.utf8Atom, c.propertyAtom, xproto.TimeCurrentTime)
		}
	case setRequest:
		if err := xproto.SetSelectionOwnerChecked(c.conn, c.win, c.clipboardAtom, xproto.TimeCurrentTime).Check(); err != nil {
			typed.result <- fmt.Errorf("x11: taking the clipboard: %w", err)
			return
		}
		c.text = typed.text
		c.owner = true
		typed.result <- nil
	}
}

// handleEvent answers an owner's requests and takes a reader's answer.
func (c *clipboard) handleEvent(event xgb.Event) {
	switch typed := event.(type) {
	case xproto.SelectionRequestEvent:
		c.answer(typed)
	case xproto.SelectionNotifyEvent:
		c.receive(typed)
	case xproto.SelectionClearEvent:
		c.owner = false
		c.text = ""
	}
}

// answer gives another client the clipboard, or refuses what it cannot have.
func (c *clipboard) answer(request xproto.SelectionRequestEvent) {
	property := request.Property
	if property == 0 {
		// An older client asks for the answer in a property named after the
		// target.
		property = request.Target
	}

	switch {
	case c.owner && request.Target == c.targetsAtom:
		targets := []xproto.Atom{c.targetsAtom, c.utf8Atom, c.textAtom, c.stringAtom}
		data := make([]byte, len(targets)*4)
		for i, target := range targets {
			xgb.Put32(data[4*i:], uint32(target))
		}
		if err := xproto.ChangePropertyChecked(c.conn, xproto.PropModeReplace, request.Requestor, property,
			c.atomAtom, 32, uint32(len(targets)), data).Check(); err != nil { //nolint:gosec // a handful of atoms.
			property = 0
		}
	case c.owner && (request.Target == c.utf8Atom || request.Target == c.textAtom || request.Target == c.stringAtom):
		data := []byte(c.text)
		target := c.utf8Atom
		if request.Target == c.stringAtom {
			target = c.stringAtom
			data = latin1(c.text)
		}
		if err := xproto.ChangePropertyChecked(c.conn, xproto.PropModeReplace, request.Requestor, property,
			target, 8, uint32(len(data)), data).Check(); err != nil { //nolint:gosec // Set bounds text to the X request limit.
			property = 0
		}
	default:
		property = 0
	}

	notify := xproto.SelectionNotifyEvent{
		Time:      request.Time,
		Requestor: request.Requestor,
		Selection: request.Selection,
		Target:    request.Target,
		Property:  property,
	}
	_ = xproto.SendEventChecked(c.conn, false, request.Requestor, 0, string(notify.Bytes())).Check()
}

// receive reads what the selection's owner answered.
func (c *clipboard) receive(event xproto.SelectionNotifyEvent) {
	if len(c.pending) == 0 || event.Requestor != c.win || event.Selection != c.clipboardAtom {
		return
	}
	if event.Property == 0 {
		if event.Target == c.utf8Atom {
			xproto.ConvertSelection(c.conn, c.win, c.clipboardAtom, c.stringAtom, c.propertyAtom, xproto.TimeCurrentTime)
		} else {
			c.finishGet(getResult{})
		}
		return
	}

	reply, err := xproto.GetProperty(c.conn, true, c.win, event.Property, 0, 0, 1<<20).Reply()
	switch {
	case err != nil:
		c.finishGet(getResult{err: fmt.Errorf("x11: reading the clipboard: %w", err)})
	case reply == nil:
		c.finishGet(getResult{err: fmt.Errorf("x11: reading the clipboard: %w", errNoReply)})
	case reply.Format != 8 || reply.BytesAfter != 0:
		c.finishGet(getResult{err: errors.New("x11: the clipboard selection is not a supported text value")})
	case reply.Type == c.stringAtom:
		text := make([]rune, len(reply.Value))
		for i, character := range reply.Value {
			text[i] = rune(character)
		}
		c.finishGet(getResult{text: string(text)})
	default:
		c.finishGet(getResult{text: string(reply.Value)})
	}
}

func (c *clipboard) finishGet(result getResult) {
	for _, waiter := range c.pending {
		waiter <- result
	}
	c.pending = nil
}

func latin1(text string) []byte {
	encoded := make([]byte, 0, len(text))
	for _, character := range text {
		if character > 255 {
			character = '?'
		}
		encoded = append(encoded, byte(character))
	}
	return encoded
}

// Get reads the clipboard.
func (c *clipboard) Get(ctx context.Context) (string, error) {
	request := getRequest{result: make(chan getResult, 1)}
	select {
	case c.requests <- request:
	case <-ctx.Done():
		return "", ctx.Err()
	case <-c.done:
		return "", errClipboardClosed
	}

	select {
	case result := <-request.result:
		return result.text, result.err
	case <-ctx.Done():
		return "", ctx.Err()
	case <-c.done:
		return "", errClipboardClosed
	}
}

// Set becomes the selection's owner and gives the clipboard the text.
func (c *clipboard) Set(ctx context.Context, text string) error {
	if len(text) > int(xproto.Setup(c.conn).MaximumRequestLength)*4-24 {
		return errors.New("x11: the clipboard text is too large for one X selection reply")
	}
	request := setRequest{text: text, result: make(chan error, 1)}
	select {
	case c.requests <- request:
	case <-ctx.Done():
		return ctx.Err()
	case <-c.done:
		return errClipboardClosed
	}

	select {
	case err := <-request.result:
		return err
	case <-ctx.Done():
		return ctx.Err()
	case <-c.done:
		return errClipboardClosed
	}
}

// close ends the loop and the connection.
func (c *clipboard) close() {
	c.stopOnce.Do(func() { close(c.stop) })
	<-c.done
}

// selection is the clipboard's connection, opened on first use.
func (c *Conn) selection(ctx context.Context) (*clipboard, error) {
	c.clipboardMu.Lock()
	defer c.clipboardMu.Unlock()
	if c.closed {
		return nil, errClipboardClosed
	}
	if c.clipboard == nil {
		clipboard, err := newClipboard(ctx, c.display)
		if err != nil {
			return nil, err
		}
		c.clipboard = clipboard
	}
	return c.clipboard, nil
}

// GetClipboard reads the desktop's clipboard as text.
func (c *Conn) GetClipboard(ctx context.Context) (string, error) {
	selection, err := c.selection(ctx)
	if err != nil {
		return "", err
	}
	return selection.Get(ctx)
}

// SetClipboard gives the desktop's clipboard text.
func (c *Conn) SetClipboard(ctx context.Context, text string) error {
	selection, err := c.selection(ctx)
	if err != nil {
		return err
	}
	return selection.Set(ctx, text)
}
