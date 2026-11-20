package cli

import (
	"context"
	"io"
)

func watchTerminalSize(context.Context, io.Writer, func(uint32, uint32)) func() {
	return func() {}
}
