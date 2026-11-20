package x11

import (
	"context"
	"errors"
	"fmt"
	"math"
	"strings"

	"github.com/jezek/xgb/xproto"
	"github.com/jezek/xgb/xtest"
)

// Button is a mouse button.
type Button int

// The buttons Rookery uses.
const (
	ButtonLeft   Button = 1
	ButtonMiddle Button = 2
	ButtonRight  Button = 3

	// The wheel is two more buttons: four up, five down.
	buttonWheelUp   Button = 4
	buttonWheelDown Button = 5
)

// Direction is which way the wheel turns.
type Direction int

// Where the wheel goes.
const (
	Up   Direction = 1
	Down Direction = 2
)

// Click moves the pointer to a pixel and clicks a button there.
func (c *Conn) Click(ctx context.Context, x, y int, button Button, count int) error {
	c.inputMu.Lock()
	defer c.inputMu.Unlock()
	if button < ButtonLeft || button > ButtonRight {
		return fmt.Errorf("x11: invalid mouse button %d", button)
	}
	if err := c.pointer(ctx, x, y); err != nil {
		return err
	}
	return c.clickButton(ctx, button, count)
}

// pointer moves the pointer to a pixel.
func (c *Conn) pointer(ctx context.Context, x, y int) error {
	if x < 0 || y < 0 || x > math.MaxInt16 || y > math.MaxInt16 {
		return fmt.Errorf("x11: (%d, %d) is not a pixel on this screen", x, y)
	}
	return c.fakeInput(ctx, xproto.MotionNotify, 0, x, y)
}

// Scroll turns the wheel at where the pointer is.
func (c *Conn) Scroll(ctx context.Context, direction Direction, count int) error {
	c.inputMu.Lock()
	defer c.inputMu.Unlock()
	button := buttonWheelUp
	if direction == Down {
		button = buttonWheelDown
	} else if direction != Up {
		return fmt.Errorf("x11: invalid scroll direction %d", direction)
	}
	return c.clickButton(ctx, button, count)
}

func (c *Conn) clickButton(ctx context.Context, button Button, count int) error {
	for range max(1, count) {
		if err := c.fakeInput(ctx, xproto.ButtonPress, byte(button), 0, 0); err != nil { //nolint:gosec // a button fits in a byte.
			return err
		}
		if err := c.fakeInput(context.WithoutCancel(ctx), xproto.ButtonRelease, byte(button), 0, 0); err != nil { //nolint:gosec // a button fits in a byte.
			return err
		}
	}
	return nil
}

// Type types text, one keysym at a time: the character as the keyboard has it,
// and a key of its own for one it does not.
func (c *Conn) Type(ctx context.Context, text string) error {
	c.inputMu.Lock()
	defer c.inputMu.Unlock()
	layout, err := c.keyboard()
	if err != nil {
		return err
	}

	for _, character := range text {
		if err := ctx.Err(); err != nil {
			return err
		}
		if err := c.withKey(ctx, layout, keysymFor(character), func(stroke keyStroke) error {
			return c.press(ctx, layout, stroke)
		}); err != nil {
			return err
		}
	}
	return nil
}

// Key presses a chord such as "ctrl+shift+t" or "Return". Its parts are
// modifiers and one key, named as X names keysyms, or a single character.
func (c *Conn) Key(ctx context.Context, chord string) error {
	c.inputMu.Lock()
	defer c.inputMu.Unlock()
	parts := strings.Split(chord, "+")
	if strings.TrimSpace(chord) == "" {
		return fmt.Errorf("x11: no key in %q", chord)
	}

	layout, err := c.keyboard()
	if err != nil {
		return err
	}

	var modifiers []keyStroke
	for _, part := range parts[:len(parts)-1] {
		keysym, ok := modifierKeysyms[strings.ToLower(strings.TrimSpace(part))]
		if !ok {
			return fmt.Errorf("x11: %q is not a modifier: want ctrl, shift, alt or super", part)
		}
		stroke, found := layout.byKeysym[keysym]
		if !found {
			return fmt.Errorf("x11: the keyboard has no %s", part)
		}
		modifiers = append(modifiers, stroke)
	}

	name := strings.TrimSpace(parts[len(parts)-1])
	keysym, found := namedKeysyms[strings.ToLower(name)]
	if !found {
		runes := []rune(name)
		if len(runes) != 1 {
			return fmt.Errorf("x11: %q is not a key: name it as X does, such as Return or F5, or give one character", name)
		}
		keysym = keysymFor(runes[0])
	}
	// Unwind the keys already pressed if the call was canceled midway.
	return c.withKey(ctx, layout, keysym, func(stroke keyStroke) (err error) {
		for _, modifier := range modifiers {
			if err := c.pressKey(ctx, modifier.code, true); err != nil {
				return err
			}
			defer func() {
				err = errors.Join(err, c.pressKey(context.WithoutCancel(ctx), modifier.code, false))
			}()
		}
		return c.press(ctx, layout, stroke)
	})
}

// keyStroke is a keycode and whether shift is held for it.
type keyStroke struct {
	code  byte
	shift bool
}

// press presses and releases a key, with shift when the character needs it.
func (c *Conn) press(ctx context.Context, layout *layout, stroke keyStroke) (err error) {
	releaseCtx := context.WithoutCancel(ctx)
	if stroke.shift {
		shift, ok := layout.byKeysym[0xffe1] // Shift_L
		if !ok {
			return fmt.Errorf("x11: the keyboard has no left shift key")
		}
		if err := c.pressKey(ctx, shift.code, true); err != nil {
			return err
		}
		defer func() { err = errors.Join(err, c.pressKey(releaseCtx, shift.code, false)) }()
	}
	if err := c.pressKey(ctx, stroke.code, true); err != nil {
		return err
	}
	return c.pressKey(releaseCtx, stroke.code, false)
}

func (c *Conn) pressKey(ctx context.Context, code byte, down bool) error {
	event := byte(xproto.KeyRelease)
	if down {
		event = xproto.KeyPress
	}
	return c.fakeInput(ctx, event, code, 0, 0)
}

// fakeInput is one XTest event.
func (c *Conn) fakeInput(ctx context.Context, event, detail byte, x, y int) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	fake := xtest.FakeInputChecked(c.conn, event, detail, xproto.TimeCurrentTime, c.root, int16(x), int16(y), 0) //nolint:gosec // pointer coordinates are checked against int16.
	if err := fake.Check(); err != nil {
		return fmt.Errorf("x11: input: %w", err)
	}
	return nil
}

// layout is the keyboard as the server has it: which keycode types which
// keysym, and whether shift is part of it.
type layout struct {
	byKeysym map[uint32]keyStroke
	spare    byte
}

// keyboard reads the keyboard's mapping once: it is the same keyboard for the
// connection's life, except for the keycode `remap` borrows for a moment.
func (c *Conn) keyboard() (*layout, error) {
	c.layoutOnce.Do(func() {
		c.layout, c.layoutErr = c.readKeyboard()
	})
	return c.layout, c.layoutErr
}

// readKeyboard is the keyboard's mapping.
func (c *Conn) readKeyboard() (*layout, error) {
	c.mu.Lock()
	setup := xproto.Setup(c.conn)
	first, count := setup.MinKeycode, setup.MaxKeycode-setup.MinKeycode+1
	reply, err := xproto.GetKeyboardMapping(c.conn, first, byte(count)).Reply() //nolint:gosec // a keyboard has at most 255 keycodes.
	c.mu.Unlock()
	if err != nil {
		return nil, fmt.Errorf("x11: reading the keyboard: %w", err)
	}
	if reply == nil {
		return nil, fmt.Errorf("x11: reading the keyboard: %w", errNoReply)
	}

	perKeycode := int(reply.KeysymsPerKeycode)
	read := &layout{byKeysym: make(map[uint32]keyStroke, len(reply.Keysyms))}
	for index := 0; index < int(count) && (index+1)*perKeycode <= len(reply.Keysyms); index++ {
		code := byte(first) + byte(index) //nolint:gosec // the keyboard has at most 255 keycodes.
		keysyms := reply.Keysyms[index*perKeycode : (index+1)*perKeycode]
		empty := true
		for level, keysym := range keysyms {
			if keysym == 0 {
				continue
			}
			empty = false
			// The first two levels are the key as it is and with shift; the
			// others need AltGr, which a chord does not ask for.
			if level > 1 {
				continue
			}
			if _, taken := read.byKeysym[uint32(keysym)]; taken {
				continue
			}
			read.byKeysym[uint32(keysym)] = keyStroke{code: code, shift: level == 1}
		}
		if empty && read.spare == 0 {
			read.spare = code
		}
	}
	return read, nil
}

// withKey borrows an unused keycode when the keyboard has no matching key.
func (c *Conn) withKey(ctx context.Context, read *layout, keysym uint32, use func(keyStroke) error) (err error) {
	if err := ctx.Err(); err != nil {
		return err
	}
	if stroke, found := read.byKeysym[keysym]; found {
		return use(stroke)
	}
	if read.spare == 0 {
		return fmt.Errorf("x11: the keyboard has no key for U+%04X and no spare keycode", keysym)
	}

	if err := c.mapKeycode(read.spare, xproto.Keysym(keysym)); err != nil {
		return fmt.Errorf("x11: borrowing a keycode for U+%04X: %w", keysym, err)
	}
	defer func() {
		if restoreErr := c.mapKeycode(read.spare, 0); restoreErr != nil {
			err = errors.Join(err, fmt.Errorf("x11: returning a borrowed keycode: %w", restoreErr))
		}
	}()
	return use(keyStroke{code: read.spare})
}

func (c *Conn) mapKeycode(code byte, keysym xproto.Keysym) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	return xproto.ChangeKeyboardMappingChecked(c.conn, 1, xproto.Keycode(code), 1, []xproto.Keysym{keysym}).Check()
}

// keysymFor is a character's keysym: Latin-1 characters are their own keysym,
// and the rest are Unicode keysyms.
func keysymFor(character rune) uint32 {
	switch character {
	case '\n', '\r':
		return 0xff0d // Return
	case '\t':
		return 0xff09 // Tab
	}
	if character >= 0x20 && character <= 0x7e {
		return uint32(character)
	}
	if character >= 0xa0 && character <= 0xff {
		return uint32(character)
	}
	return 0x01000000 | uint32(character) //nolint:gosec // a keysym is a rune's number.
}

// modifierKeysyms are the chord names and the keysyms they hold down.
var modifierKeysyms = map[string]uint32{
	"ctrl":    0xffe3, // Control_L
	"control": 0xffe3,
	"shift":   0xffe1, // Shift_L
	"alt":     0xffe9, // Alt_L
	"meta":    0xffe9,
	"super":   0xffeb, // Super_L
	"win":     0xffeb,
}

// namedKeysyms are the key names a chord may use, in X's spelling.
var namedKeysyms = map[string]uint32{
	"return":    0xff0d,
	"enter":     0xff0d,
	"tab":       0xff09,
	"escape":    0xff1b,
	"esc":       0xff1b,
	"backspace": 0xff08,
	"delete":    0xffff,
	"del":       0xffff,
	"insert":    0xff63,
	"space":     0x0020,
	"home":      0xff50,
	"end":       0xff57,
	"pageup":    0xff55,
	"page_up":   0xff55,
	"prior":     0xff55,
	"pagedown":  0xff56,
	"page_down": 0xff56,
	"next":      0xff56,
	"up":        0xff52,
	"down":      0xff54,
	"left":      0xff51,
	"right":     0xff53,
	"f1":        0xffbe,
	"f2":        0xffbf,
	"f3":        0xffc0,
	"f4":        0xffc1,
	"f5":        0xffc2,
	"f6":        0xffc3,
	"f7":        0xffc4,
	"f8":        0xffc5,
	"f9":        0xffc6,
	"f10":       0xffc7,
	"f11":       0xffc8,
	"f12":       0xffc9,
}
