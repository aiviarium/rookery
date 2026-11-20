// A workstation's desktop, as noVNC draws it from `/ws/hosts/{name}/desktop`.
//
// The route carries RFB bytes unread, so the VNC password never reaches this
// page: the server answers the host's challenge. The clipboard is the desktop's,
// read and written through DesktopService; a screenshot is a PNG the same service
// takes for anyone who may view the host.
import { Show, createSignal, onCleanup, onSettled } from "solid-js";
import type { JSX } from "@solidjs/web";
import { useParams } from "@solidjs/router";
import RFB from "@novnc/novnc";
import { Button, Callout, Card, Dialog, State, TextArea, type DotStatus } from "@aiviarium/nocturne";

import { useShell } from "../../../shell.tsx";
import { Page } from "../../../parts/page.tsx";
import { errorMessage } from "../format.ts";

type DesktopState = "connecting" | "connected" | "lost" | "ended";

const TONE: Record<DesktopState, DotStatus> = {
  connecting: "idle",
  connected: "live",
  lost: "warn",
  ended: "neutral",
};

const WORD: Record<DesktopState, string> = {
  connecting: "Connecting",
  connected: "Live",
  lost: "Reconnecting",
  ended: "Ended",
};

export function DesktopPage(): JSX.Element {
  const shell = useShell();
  const params = useParams<{ name: string }>();
  const host = () => params.name ?? "";

  let screen!: HTMLDivElement;
  let stage!: HTMLDivElement;
  let rfb: RFB | undefined;
  let retry: ReturnType<typeof setTimeout> | undefined;
  let attempts = 0;
  let disposed = false;

  const [state, setState] = createSignal<DesktopState>("connecting");
  const [detail, setDetail] = createSignal<string>();
  const [paused, setPaused] = createSignal(false);
  const [clipboard, setClipboard] = createSignal("");
  const [clipboardOpen, setClipboardOpen] = createSignal(false);
  const [shot, setShot] = createSignal<string>();
  const [shotOpen, setShotOpen] = createSignal(false);
  const [failure, setFailure] = createSignal<string>();

  const connect = (): void => {
    if (disposed) return;
    setState("connecting");
    const opened = new RFB(screen, shell.rookery.desktop.url(host()), { wsProtocols: ["binary"] });
    rfb = opened;
    opened.scaleViewport = true;
    opened.resizeSession = false;
    opened.focusOnClick = true;
    opened.background = getComputedStyle(document.documentElement).getPropertyValue("--bg-0").trim();
    opened.addEventListener("connect", () => {
      attempts = 0;
      setState("connected");
      setDetail(undefined);
    });
    opened.addEventListener("disconnect", (event: Event) => {
      const clean = (event as CustomEvent<{ clean?: boolean }>).detail?.clean ?? false;
      rfb = undefined;
      if (disposed) return;
      if (clean) {
        setState("ended");
        return;
      }
      setState("lost");
      attempts += 1;
      retry = setTimeout(connect, Math.min(10_000, 1000 * 2 ** (attempts - 1)));
    });
    opened.addEventListener("securityfailure", (event: Event) => {
      setState("ended");
      setDetail((event as CustomEvent<{ reason?: string }>).detail?.reason ?? "The desktop refused the connection.");
    });
    opened.addEventListener("clipboard", (event: Event) => {
      const text = (event as CustomEvent<{ text?: string }>).detail?.text;
      if (typeof text === "string") setClipboard(text);
    });
  };

  onSettled(() => {
    connect();
    onCleanup(() => {
      disposed = true;
      if (retry !== undefined) clearTimeout(retry);
      rfb?.disconnect();
    });
  });

  const fullscreen = async (): Promise<void> => {
    try {
      if (document.fullscreenElement === null) await stage.requestFullscreen();
      else await document.exitFullscreen();
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const screenshot = async (): Promise<void> => {
    setFailure(undefined);
    try {
      const taken = await shell.rookery.desktop.screenshot({ host: host() });
      if (taken.png.length === 0) throw new Error("The desktop answered with no picture.");
      const url = URL.createObjectURL(new Blob([taken.png.slice()], { type: "image/png" }));
      setShot(previous => {
        if (previous !== undefined) URL.revokeObjectURL(previous);
        return url;
      });
      setShotOpen(true);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const sendClipboard = async (): Promise<void> => {
    setFailure(undefined);
    try {
      await shell.rookery.desktop.setClipboard({ host: host(), text: clipboard() });
      rfb?.clipboardPasteFrom(clipboard());
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const readClipboard = async (): Promise<void> => {
    setFailure(undefined);
    try {
      const read = await shell.rookery.desktop.getClipboard({ host: host() });
      setClipboard(read.text);
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  const togglePause = async (): Promise<void> => {
    setFailure(undefined);
    try {
      if (paused()) await shell.rookery.desktop.resume({ host: host() });
      else await shell.rookery.desktop.pause({ host: host() });
      setPaused(!paused());
    } catch (error) {
      setFailure(errorMessage(error));
    }
  };

  return (
    <Page
      title={`Desktop of ${host()}`}
      crumbs={() => [{ label: "Hosts", href: "/hosts" }, { label: host() }, { label: "Desktop" }]}
      description="The workstation's shared desktop, drawn in this tab. Input and the clipboard go through the server."
      context={<State status={TONE[state()]}>{WORD[state()]}</State>}
      actions={() => [
        { id: "clipboard", label: "Clipboard", icon: "copy", onClick: () => setClipboardOpen(!clipboardOpen()) },
        {
          id: "pause",
          label: paused() ? "Resume" : "Pause",
          icon: paused() ? "play" : "pause",
          onClick: () => void togglePause(),
        },
        { id: "screenshot", label: "Screenshot", icon: "eye", onClick: () => void screenshot() },
        { id: "fullscreen", label: "Full screen", primary: true, onClick: () => void fullscreen() },
      ]}
    >
      <Show when={failure()}>{message => <Callout tone="danger">{message()}</Callout>}</Show>
      <Show when={clipboardOpen()}>
        <Card title="Clipboard">
          <div class="nc-stack">
            <TextArea
              label="Clipboard"
              rows={3}
              value={clipboard()}
              onInput={event => setClipboard(event.currentTarget.value)}
            />
            <div class="nc-row">
              <Button onClick={() => void sendClipboard()}>Send to the desktop</Button>
              <Button onClick={() => void readClipboard()}>Read from the desktop</Button>
            </div>
          </div>
        </Card>
      </Show>
      <div class="shell-desktop" data-part="desktop-page" data-host={host()} data-state={state()} ref={stage}>
        <div class="shell-desktop__screen" ref={screen} />
        <Show when={state() !== "connected"}>
          <div class="shell-desktop__overlay">
            <State status={TONE[state()]}>{detail() ?? `The desktop of ${host()}`}</State>
          </div>
        </Show>
      </div>
      <Dialog
        open={shotOpen()}
        onClose={() => setShotOpen(false)}
        title={`Screenshot of ${host()}`}
        size="lg"
        footer={
          <>
            <Button onClick={() => setShotOpen(false)}>Close</Button>
            <Show when={shot()}>
              {url => (
                <Button
                  onClick={() => {
                    const anchor = document.createElement("a");
                    anchor.href = url();
                    anchor.download = `${host()}.png`;
                    anchor.click();
                  }}
                >
                  Download
                </Button>
              )}
            </Show>
          </>
        }
      >
        <Show when={shot()}>{url => <img class="shell-shot" src={url()} alt={`The desktop of ${host()}`} />}</Show>
      </Dialog>
    </Page>
  );
}
