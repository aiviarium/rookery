package boxcli

import (
	"context"
	"fmt"
	"io"
	"os"
	"strconv"
	"strings"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/agent"
)

// runDesktop is `box desktop`: the desktop of the host box runs on.
func runDesktop(ctx context.Context, options Options, args []string) error {
	if len(args) == 0 {
		return usage("usage: box desktop screenshot|click|type|key|scroll|clipboard|pause|resume|state|serve")
	}

	switch args[0] {
	case "serve":
		return runDesktopServe(ctx, options, args[1:])
	case "screenshot":
		return runDesktopScreenshot(ctx, options, args[1:])
	case "click":
		return runDesktopClick(ctx, options, args[1:])
	case "type":
		return runDesktopType(ctx, options, args[1:])
	case "key":
		return runDesktopKey(ctx, options, args[1:])
	case "scroll":
		return runDesktopScroll(ctx, options, args[1:])
	case "clipboard":
		return runDesktopClipboard(ctx, options, args[1:])
	case "pause", "resume", "state":
		return runDesktopSimple(ctx, options, args)
	default:
		return usage("box desktop: there is no %q", args[0])
	}
}

// desktopClient is a client and the invitation to close it, for the one call a
// desktop command makes.
func desktopClient(options Options) (*agent.Client, error) { return dial(options) }

func runDesktopScreenshot(ctx context.Context, options Options, args []string) error {
	output, _, err := takeFlag(args, "--output")
	if err != nil {
		return err
	}

	client, err := desktopClient(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	response, err := client.Desktop.Screenshot(ctx, connect.NewRequest(&hostv1.DesktopServiceScreenshotRequest{}))
	if err != nil {
		return connectError("screenshot", err)
	}
	png := response.Msg.GetPng()

	if output == "" || output == "-" {
		_, err = options.Stdout.Write(png)
		return err
	}
	if err := os.WriteFile(output, png, 0o600); err != nil { //nolint:gosec // a screenshot is not a secret, but 0600 is a fine default.
		return fmt.Errorf("box: writing %s: %w", output, err)
	}
	return nil
}

func runDesktopClick(ctx context.Context, options Options, args []string) error {
	if len(args) < 2 || len(args) > 3 {
		return usage("usage: box desktop click X Y [left|middle|right]")
	}
	x, err := strconv.Atoi(args[0])
	if err != nil {
		return usage("box desktop click: %q is not a pixel", args[0])
	}
	y, err := strconv.Atoi(args[1])
	if err != nil {
		return usage("box desktop click: %q is not a pixel", args[1])
	}

	button := hostv1.MouseButton_MOUSE_BUTTON_LEFT
	if len(args) == 3 {
		switch strings.ToLower(args[2]) {
		case "left":
			button = hostv1.MouseButton_MOUSE_BUTTON_LEFT
		case "middle":
			button = hostv1.MouseButton_MOUSE_BUTTON_MIDDLE
		case "right":
			button = hostv1.MouseButton_MOUSE_BUTTON_RIGHT
		default:
			return usage("box desktop click: %q is not a button: want left, middle or right", args[2])
		}
	}

	return sendDesktopInput(ctx, options, &hostv1.DesktopServiceInputRequest{
		Input: &hostv1.DesktopServiceInputRequest_Click{Click: &hostv1.DesktopClick{
			X: int32(x), Y: int32(y), Button: button, //nolint:gosec // a pixel is small.
		}},
	})
}

func runDesktopType(ctx context.Context, options Options, args []string) error {
	if len(args) == 0 {
		return usage("usage: box desktop type TEXT")
	}
	return sendDesktopInput(ctx, options, &hostv1.DesktopServiceInputRequest{
		Input: &hostv1.DesktopServiceInputRequest_Text{Text: &hostv1.DesktopText{Text: strings.Join(args, " ")}},
	})
}

func runDesktopKey(ctx context.Context, options Options, args []string) error {
	if len(args) != 1 {
		return usage("usage: box desktop key CHORD")
	}
	return sendDesktopInput(ctx, options, &hostv1.DesktopServiceInputRequest{
		Input: &hostv1.DesktopServiceInputRequest_Key{Key: &hostv1.DesktopKey{Chord: args[0]}},
	})
}

func runDesktopScroll(ctx context.Context, options Options, args []string) error {
	if len(args) == 0 || len(args) > 2 {
		return usage("usage: box desktop scroll up|down [N]")
	}

	var direction hostv1.ScrollDirection
	switch strings.ToLower(args[0]) {
	case "up":
		direction = hostv1.ScrollDirection_SCROLL_DIRECTION_UP
	case "down":
		direction = hostv1.ScrollDirection_SCROLL_DIRECTION_DOWN
	default:
		return usage("box desktop scroll: %q is not a direction: want up or down", args[0])
	}

	count := 3
	if len(args) == 2 {
		parsed, err := strconv.Atoi(args[1])
		if err != nil || parsed < 1 {
			return usage("box desktop scroll: %q is not a count", args[1])
		}
		count = parsed
	}

	return sendDesktopInput(ctx, options, &hostv1.DesktopServiceInputRequest{
		Input: &hostv1.DesktopServiceInputRequest_Scroll{Scroll: &hostv1.DesktopScroll{
			Direction: direction,
			Count:     uint32(count), //nolint:gosec // a count is positive.
		}},
	})
}

// sendDesktopInput makes one input call.
func sendDesktopInput(ctx context.Context, options Options, request *hostv1.DesktopServiceInputRequest) error {
	client, err := desktopClient(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	if _, err := client.Desktop.Input(ctx, connect.NewRequest(request)); err != nil {
		return connectError("desktop input", err)
	}
	return nil
}

func runDesktopClipboard(ctx context.Context, options Options, args []string) error {
	set := false
	rest := make([]string, 0, len(args))
	for _, arg := range args {
		if arg == "--set" {
			set = true
			continue
		}
		rest = append(rest, arg)
	}

	client, err := desktopClient(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	if !set {
		if len(rest) != 0 {
			return usage("usage: box desktop clipboard [--set [TEXT]]")
		}
		response, err := client.Desktop.GetClipboard(ctx, connect.NewRequest(&hostv1.DesktopServiceGetClipboardRequest{}))
		if err != nil {
			return connectError("clipboard", err)
		}
		_, err = fmt.Fprint(options.Stdout, response.Msg.GetText())
		return err
	}

	text := strings.Join(rest, " ")
	if text == "" {
		// The text may come from stdin, which is how a script passes a lot of it
		// without it appearing in a process's arguments.
		raw, err := io.ReadAll(options.Stdin)
		if err != nil {
			return fmt.Errorf("box: reading the text: %w", err)
		}
		text = string(raw)
	}
	if _, err := client.Desktop.SetClipboard(ctx, connect.NewRequest(&hostv1.DesktopServiceSetClipboardRequest{Text: text})); err != nil {
		return connectError("clipboard", err)
	}
	return nil
}

// runDesktopSimple runs pause, resume or state.
func runDesktopSimple(ctx context.Context, options Options, args []string) error {
	client, err := desktopClient(options)
	if err != nil {
		return err
	}
	defer func() { _ = client.Close() }()

	switch args[0] {
	case "pause":
		if _, err := client.Desktop.Pause(ctx, connect.NewRequest(&hostv1.DesktopServicePauseRequest{})); err != nil {
			return connectError("pause", err)
		}
		return nil
	case "resume":
		if _, err := client.Desktop.Resume(ctx, connect.NewRequest(&hostv1.DesktopServiceResumeRequest{})); err != nil {
			return connectError("resume", err)
		}
		return nil
	default:
		response, err := client.Desktop.State(ctx, connect.NewRequest(&hostv1.DesktopServiceStateRequest{}))
		if err != nil {
			return connectError("state", err)
		}
		return writeJSON(options.Stdout, desktopState(response.Msg.GetDesktop()))
	}
}

// desktopState is a desktop's state as box prints it.
func desktopState(state *hostv1.DesktopState) map[string]any {
	return map[string]any{
		"available": state.GetAvailable(),
		"paused":    state.GetPaused(),
		"display":   state.GetDisplay(),
		"user":      state.GetUser(),
		"width":     state.GetWidth(),
		"height":    state.GetHeight(),
		"reason":    state.GetReason(),
	}
}
