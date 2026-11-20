// One terminal, drawn by xterm.js over `/ws/terminals/{id}`.
//
// The bytes are the PTY's: the host keeps the screen, and this draws it. A socket
// that drops reattaches from the byte it last had, so the screen keeps what it
// showed instead of starting again.
import { browserSocket, type TerminalSession } from "@rookery/client";
import { Show, createEffect, createSignal, onCleanup, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { FitAddon } from "@xterm/addon-fit";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";

import { State, type DotStatus } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";

type PaneState = "connecting" | "live" | "ended" | "error";

const TONE: Record<PaneState, DotStatus> = {
  connecting: "idle",
  live: "live",
  ended: "neutral",
  error: "error",
};

const WORD: Record<PaneState, string> = {
  connecting: "Connecting",
  live: "Live",
  ended: "Ended",
  error: "Reconnecting",
};

export function TerminalPane(props: { id: string; active: boolean }): JSX.Element {
  const shell = useShell();
  let frame!: HTMLDivElement;
  const [state, setState] = createSignal<PaneState>("connecting");
  const [exited, setExited] = createSignal<number>();
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let offset = 0;
  let attempts = 0;
  let closed = false;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let session: TerminalSession | undefined;

  const css = (name: string): string =>
    typeof document === "undefined" ? "" : getComputedStyle(document.documentElement).getPropertyValue(name).trim();

  const connect = (): void => {
    if (closed || term === undefined) return;
    const opened = shell.rookery.terminals.attach(props.id, {
      // A reattach starts at the byte this pane already has, so the screen is
      // continued rather than replayed from the beginning.
      factory: (url: string) => browserSocket(offset > 0 ? `${url}?offset=${offset}` : url),
    });
    session = opened;
    opened.onData(bytes => {
      offset += bytes.length;
      term?.write(bytes);
    });
    opened.onExit(code => {
      setExited(code);
      setState("ended");
      term?.write(`\r\n[exited ${code}]\r\n`);
    });
    const stop = opened.status.subscribe(status => {
      if (status === "open") {
        attempts = 0;
        setState("live");
        opened.resize(term?.cols ?? 80, term?.rows ?? 24);
        return;
      }
      if (status !== "closed" && status !== "error") return;
      stop();
      if (closed || exited() !== undefined) {
        if (!closed) setState("ended");
        return;
      }
      setState("error");
      attempts += 1;
      retry = setTimeout(connect, Math.min(10_000, 500 * 2 ** (attempts - 1)));
    });
    opened.open();
  };

  onSettled(() => {
    const terminal = new Terminal({
      allowProposedApi: true,
      fontFamily: css("--font-mono") || "monospace",
      fontSize: 12,
      lineHeight: 1.2,
      cursorBlink: true,
      scrollback: 10_000,
      macOptionIsMeta: true,
      theme: {
        background: css("--bg-0"),
        foreground: css("--text-1"),
        cursor: css("--accent-strong"),
        cursorAccent: css("--bg-0"),
        selectionBackground: css("--accent-tint"),
        black: css("--bg-0"),
        red: css("--danger"),
        green: css("--success"),
        yellow: css("--warning"),
        blue: css("--info"),
        magenta: css("--label-orchid"),
        cyan: css("--label-teal"),
        white: css("--text-1"),
        brightBlack: css("--text-4"),
        brightRed: css("--danger"),
        brightGreen: css("--success"),
        brightYellow: css("--warning"),
        brightBlue: css("--info"),
        brightMagenta: css("--label-orchid"),
        brightCyan: css("--label-teal"),
        brightWhite: css("--text-1"),
      },
    });
    const size = new FitAddon();
    terminal.loadAddon(size);
    terminal.open(frame);
    term = terminal;
    fit = size;

    terminal.onData(data => session?.write(data));

    const measure = new ResizeObserver(() => {
      if (!props.active) return;
      try {
        size.fit();
      } catch {
        // A pane that is being hidden has no size to fit to.
      }
      session?.resize(terminal.cols, terminal.rows);
    });
    measure.observe(frame);
    queueMicrotask(() => {
      try {
        size.fit();
      } catch {
        // The pane is not laid out yet; the effect below fits it when it is.
      }
      connect();
    });

    onCleanup(() => {
      closed = true;
      if (retry !== undefined) clearTimeout(retry);
      measure.disconnect();
      session?.close();
      terminal.dispose();
    });
  });

  // A tab that comes back fits again: it had no size while it was hidden.
  createEffect(
    () => props.active,
    (active) => {
      if (!active) return;
      queueMicrotask(() => {
        if (term === undefined || fit === undefined) return;
        try {
          fit.fit();
        } catch {
          return;
        }
        session?.resize(term.cols, term.rows);
        term.focus();
      });
    },
  );

  return (
    <div class="infra-terminal" data-part="terminal" data-terminal={props.id} data-state={state()}>
      <div class="nc-row infra-terminal__state">
        <State status={TONE[state()]}>{exited() === undefined ? WORD[state()] : `Exited with ${exited()}`}</State>
        <span class="nc-meta nc-code">{props.id}</span>
      </div>
      <div class="infra-terminal__frame" ref={frame} />
      <Show when={state() === "error"}>
        <p class="nc-meta">The connection dropped. The terminal reattaches from where it stopped.</p>
      </Show>
    </div>
  );
}
