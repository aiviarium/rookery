package x11

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"image"
	"image/png"
	"io"
	"log"
	"os"
	"os/exec"
	"testing"
	"time"

	"github.com/jezek/xgb"
	"github.com/jezek/xgb/xproto"
)

// TestMain silences xgb's own log, which says when it could not read an
// authority file and when a connection ended under it: both are normal here,
// where the server is killed by a test.
func TestMain(m *testing.M) {
	xgb.Logger = log.New(io.Discard, "", 0)
	os.Exit(m.Run())
}

// openTestDisplay starts an Xvfb and returns a connection to it, skipping the
// test when Xvfb is not installed.
func openTestDisplay(t *testing.T, width, height int) *Conn {
	t.Helper()

	if _, err := exec.LookPath("Xvfb"); err != nil {
		t.Skip("x11: Xvfb is not installed")
	}

	display, stop := startXvfb(t, width, height)
	t.Cleanup(stop)

	conn, err := Open(context.Background(), display)
	if err != nil {
		t.Fatalf("x11: opening %s: %v", display, err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	return conn
}

// startXvfb runs a server on a display number of its own and returns its name and
// a function that stops it.
func startXvfb(t *testing.T, width, height int) (string, func()) {
	t.Helper()

	var lastErr error
	for attempt := 0; attempt < 5; attempt++ {
		var random [2]byte
		if _, err := rand.Read(random[:]); err != nil {
			t.Fatalf("x11: choosing a display: %v", err)
		}
		display := fmt.Sprintf(":%d", 200+int(binary.BigEndian.Uint16(random[:])%2000))

		cmd := exec.CommandContext(context.Background(), "Xvfb", display, "-screen", "0", fmt.Sprintf("%dx%dx24", width, height), //nolint:gosec // a test's own X server.
			"-nolisten", "tcp", "-noreset")
		var output bytes.Buffer
		cmd.Stderr = &output
		if err := cmd.Start(); err != nil {
			t.Fatalf("x11: starting Xvfb: %v", err)
		}
		stop := func() {
			_ = cmd.Process.Kill()
			_ = cmd.Wait()
		}

		// The socket is there before the server answers, so the test opens
		// connections until one works.
		deadline := time.Now().Add(10 * time.Second)
		for time.Now().Before(deadline) {
			conn, err := Open(context.Background(), display)
			if err == nil {
				_ = conn.Close()
				return display, stop
			}
			lastErr = err
			time.Sleep(20 * time.Millisecond)
		}
		stop()
	}
	t.Fatalf("x11: no Xvfb answered: %v", lastErr)
	return "", nil
}

func TestSizeAndScreenshot(t *testing.T) {
	t.Parallel()

	conn := openTestDisplay(t, 320, 200)
	width, height := conn.Size()
	if width != 320 || height != 200 {
		t.Fatalf("the screen is %dx%d, want 320x200", width, height)
	}

	picture, err := conn.Screenshot(context.Background())
	if err != nil {
		t.Fatalf("screenshot: %v", err)
	}
	if bounds := picture.Bounds(); bounds.Dx() != 320 || bounds.Dy() != 200 {
		t.Errorf("the screenshot is %v, want 320x200", bounds)
	}

	var encoded bytes.Buffer
	if err := png.Encode(&encoded, picture); err != nil {
		t.Fatalf("encoding the screenshot: %v", err)
	}
	if _, err := png.Decode(bytes.NewReader(encoded.Bytes())); err != nil {
		t.Fatalf("decoding the screenshot: %v", err)
	}
}

func TestClickMovesThePointer(t *testing.T) {
	t.Parallel()

	conn := openTestDisplay(t, 320, 200)
	if err := conn.Click(context.Background(), 120, 90, ButtonLeft, 1); err != nil {
		t.Fatalf("click: %v", err)
	}

	reply, err := xproto.QueryPointer(conn.conn, conn.root).Reply()
	if err != nil {
		t.Fatalf("asking where the pointer is: %v", err)
	}
	if reply.RootX != 120 || reply.RootY != 90 {
		t.Errorf("the pointer is at (%d, %d), want (120, 90)", reply.RootX, reply.RootY)
	}

	if err := conn.Scroll(context.Background(), Down, 3); err != nil {
		t.Fatalf("scroll: %v", err)
	}
}

func TestTypingReachesAWindow(t *testing.T) {
	t.Parallel()

	conn := openTestDisplay(t, 320, 200)

	windowID, err := xproto.NewWindowId(conn.conn)
	if err != nil {
		t.Fatalf("making a window: %v", err)
	}
	if err := xproto.CreateWindowChecked(conn.conn, 0, windowID, conn.root, 0, 0, 100, 100, 0,
		xproto.WindowClassInputOutput, 0, xproto.CwEventMask,
		[]uint32{xproto.EventMaskKeyPress | xproto.EventMaskButtonPress}).Check(); err != nil {
		t.Fatalf("creating a window: %v", err)
	}
	if err := xproto.MapWindowChecked(conn.conn, windowID).Check(); err != nil {
		t.Fatalf("mapping a window: %v", err)
	}
	if err := xproto.SetInputFocusChecked(conn.conn, xproto.InputFocusParent, windowID, xproto.TimeCurrentTime).Check(); err != nil {
		t.Fatalf("focusing a window: %v", err)
	}
	// A round trip makes sure the server has done all of that before the keys
	// are typed.
	if _, err := xproto.GetInputFocus(conn.conn).Reply(); err != nil {
		t.Fatalf("settling: %v", err)
	}

	if err := conn.Type(context.Background(), "a"); err != nil {
		t.Fatalf("typing: %v", err)
	}
	if err := conn.Key(context.Background(), "Return"); err != nil {
		t.Fatalf("pressing Return: %v", err)
	}

	// The window selected KeyPress on this connection, so the events arrive here.
	pressed := make(chan string, 4)
	go func() {
		for {
			event, err := conn.conn.WaitForEvent()
			if err != nil {
				return
			}
			if key, ok := event.(xproto.KeyPressEvent); ok {
				pressed <- string(rune(key.Detail))
			}
		}
	}()

	deadline := time.After(10 * time.Second)
	var seen int
	for seen < 2 {
		select {
		case <-pressed:
			seen++
		case <-deadline:
			t.Fatalf("the window saw %d key presses, want 2", seen)
		}
	}
}

func TestClipboardBetweenConnections(t *testing.T) {
	t.Parallel()

	conn := openTestDisplay(t, 320, 200)
	other, err := Open(context.Background(), conn.Display())
	if err != nil {
		t.Fatalf("opening a second connection: %v", err)
	}
	defer func() { _ = other.Close() }()

	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Second)
	defer cancel()

	if empty, err := conn.GetClipboard(ctx); err != nil || empty != "" {
		t.Fatalf("a fresh display's clipboard is %q, %v", empty, err)
	}

	if err := conn.SetClipboard(ctx, "hello clipboard"); err != nil {
		t.Fatalf("setting the clipboard: %v", err)
	}
	if got, err := other.GetClipboard(ctx); err != nil || got != "hello clipboard" {
		t.Fatalf("another connection read %q, %v", got, err)
	}

	if err := other.SetClipboard(ctx, "the other one"); err != nil {
		t.Fatalf("setting the clipboard from the other connection: %v", err)
	}
	if got, err := conn.GetClipboard(ctx); err != nil || got != "the other one" {
		t.Fatalf("the first connection read %q, %v", got, err)
	}
}

func TestScale(t *testing.T) {
	t.Parallel()

	source := image.NewNRGBA(image.Rect(0, 0, 100, 50))
	if scaled := Scale(source, 0, 0); scaled != source {
		t.Error("Scale with no size changed the image")
	}
	if scaled := Scale(source, 50, 0); scaled.Bounds().Dx() != 50 || scaled.Bounds().Dy() != 25 {
		t.Errorf("Scale to a width of 50 gave %v", scaled.Bounds())
	}
	if scaled := Scale(source, 0, 10); scaled.Bounds().Dx() != 20 || scaled.Bounds().Dy() != 10 {
		t.Errorf("Scale to a height of 10 gave %v", scaled.Bounds())
	}
}
