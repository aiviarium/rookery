package hosts

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httputil"
	"net/url"
	"path"
	"strconv"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/coder/websocket"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// RegisterRoutes mounts the browser terminal and port protocols on the edge.
func (r *Runtime) RegisterRoutes(mux *http.ServeMux) {
	mux.HandleFunc("GET /ws/terminals/{id}", r.serveTerminal)
	mux.HandleFunc("GET /ws/hosts/{name}/ports/{port}", r.servePortSocket)
	mux.HandleFunc("/hosts/{name}/ports/{port}", r.servePortHTTP)
	mux.HandleFunc("/hosts/{name}/ports/{port}/{rest...}", r.servePortHTTP)
}

// The terminal protocol uses binary messages for PTY bytes and JSON control
// messages for resize and exit. An optional offset starts the replay.
func (r *Runtime) serveTerminal(w http.ResponseWriter, request *http.Request) {
	id := request.PathValue("id")
	if _, err := r.terminal(request.Context(), id, false); err != nil {
		hostHTTPError(w, err)
		return
	}
	first := &hostv1.SessionServiceAttachRequest{Id: id}
	if offset := request.URL.Query().Get("offset"); offset != "" {
		var err error
		first.Offset, err = strconv.ParseUint(offset, 10, 64)
		if err != nil {
			hostHTTPError(w, access.ErrInvalid)
			return
		}
	}
	socket, err := websocket.Accept(w, request, nil)
	if err != nil {
		return
	}
	defer func() { _ = socket.CloseNow() }()
	ctx := request.Context()
	service := &TerminalService{r}
	err = service.relay(ctx, first, func() (*hostv1.SessionServiceAttachRequest, error) {
		kind, data, err := socket.Read(ctx)
		if err != nil {
			return nil, err
		}
		switch kind {
		case websocket.MessageBinary:
			return &hostv1.SessionServiceAttachRequest{Input: data}, nil
		case websocket.MessageText:
			var control struct {
				Type string `json:"type"`
				Cols uint32 `json:"cols"`
				Rows uint32 `json:"rows"`
			}
			if err := json.Unmarshal(data, &control); err != nil || control.Type != "resize" ||
				control.Cols == 0 || control.Cols > 65535 || control.Rows == 0 || control.Rows > 65535 {
				return nil, fmt.Errorf("%w: invalid terminal resize message", access.ErrInvalid)
			}
			return &hostv1.SessionServiceAttachRequest{Cols: control.Cols, Rows: control.Rows}, nil
		default:
			return nil, fmt.Errorf("%w: unknown terminal frame", access.ErrInvalid)
		}
	}, func(event *hostv1.SessionServiceAttachResponse) error {
		if event.GetTruncated() {
			data, err := json.Marshal(struct {
				Type   string `json:"type"`
				Offset uint64 `json:"offset"`
			}{"truncated", event.GetOffset()})
			if err != nil {
				return err
			}
			if err := socket.Write(ctx, websocket.MessageText, data); err != nil {
				return err
			}
		}
		if len(event.GetOutput()) != 0 {
			if err := socket.Write(ctx, websocket.MessageBinary, event.GetOutput()); err != nil {
				return err
			}
		}
		switch {
		case event.GetDropped():
			return socket.Write(ctx, websocket.MessageText, []byte(`{"type":"dropped"}`))
		case event.GetExited():
			return socket.Write(ctx, websocket.MessageText, fmt.Appendf(nil, `{"type":"exit","code":%d}`, event.GetExitCode()))
		}
		return nil
	})
	closeWebSocket(socket, err)
}

func (r *Runtime) servePortSocket(w http.ResponseWriter, request *http.Request) {
	ctx := request.Context()
	host, _, target, err := r.httpPort(request)
	if err != nil {
		hostHTTPError(w, err)
		return
	}
	// Validate WebSocket origin before dialing a target controlled by the URL.
	socket, err := websocket.Accept(w, request, nil)
	if err != nil {
		return
	}
	defer func() { _ = socket.CloseNow() }()
	conn, err := r.hosts.deps.Links.DialPort(ctx, host, target)
	if err != nil {
		closeWebSocket(socket, err)
		return
	}
	defer func() { _ = conn.Close() }()
	remote := websocket.NetConn(ctx, socket, websocket.MessageBinary)
	pumped := make(chan error, 1)
	go func() {
		_, err := io.Copy(conn, remote)
		_ = conn.Close()
		pumped <- err
	}()
	_, err = io.Copy(remote, conn)
	_ = remote.Close()
	inputErr := <-pumped
	closeWebSocket(socket, errors.Join(err, inputErr))
}

func (r *Runtime) servePortHTTP(w http.ResponseWriter, request *http.Request) {
	// A browser may send the session cookie by itself on a cross-site request.
	if site := request.Header.Get("Sec-Fetch-Site"); site != "" && site != "same-origin" && site != "none" {
		hostHTTPError(w, access.ErrDenied)
		return
	}
	if origin := request.Header.Get("Origin"); origin != "" {
		parsed, err := url.Parse(origin)
		if err != nil || (parsed.Scheme != "http" && parsed.Scheme != "https") || !strings.EqualFold(parsed.Host, request.Host) {
			hostHTTPError(w, access.ErrDenied)
			return
		}
	}
	host, port, target, err := r.httpPort(request)
	if err != nil {
		hostHTTPError(w, err)
		return
	}
	prefix := "/hosts/" + url.PathEscape(host) + "/ports/" + port
	upstream := &url.URL{Scheme: "http", Host: net.JoinHostPort("127.0.0.1", port)}
	ctx := request.Context()
	transport := &http.Transport{
		DisableKeepAlives:     true,
		ResponseHeaderTimeout: 30 * time.Second,
		DialContext: func(dialCtx context.Context, _, _ string) (net.Conn, error) {
			bridgeCtx, cancel := context.WithCancel(dialCtx)
			stop := context.AfterFunc(ctx, cancel)
			conn, err := r.hosts.deps.Links.DialPort(bridgeCtx, host, target)
			if err != nil {
				stop()
				cancel()
				return nil, err
			}
			return &proxyConn{Conn: conn, cancel: func() { stop(); cancel() }}, nil
		},
	}
	proxy := &httputil.ReverseProxy{
		Transport: transport,
		Rewrite: func(out *httputil.ProxyRequest) {
			out.SetURL(upstream)
			out.Out.URL.Path = "/" + out.In.PathValue("rest")
			rawPath := strings.TrimPrefix(out.In.URL.EscapedPath(), prefix)
			if strings.HasPrefix(rawPath, "/") {
				if decoded, err := url.PathUnescape(rawPath); err == nil && decoded == out.Out.URL.Path {
					out.Out.URL.RawPath = rawPath
				}
			}
			for _, header := range []string{"Authorization", "Cookie", "Rookery-CSRF", "Rookery-Caller", "Rookery-Space", "Upgrade"} {
				out.Out.Header.Del(header)
			}
		},
		ModifyResponse: func(response *http.Response) error {
			for _, header := range []string{"Set-Cookie", "Clear-Site-Data", "Alt-Svc", "Strict-Transport-Security",
				"WWW-Authenticate", "Proxy-Authenticate", "Service-Worker-Allowed", "Refresh", "Access-Control-Allow-Origin",
				"Access-Control-Allow-Credentials"} {
				response.Header.Del(header)
			}
			response.Header.Set("Cache-Control", "private, no-store")
			response.Header.Set("X-Content-Type-Options", "nosniff")
			// Upstream scripts may run with an opaque origin. They cannot
			// read the shell's storage or use authenticated edge requests.
			response.Header.Add("Content-Security-Policy", "sandbox allow-scripts allow-forms; base-uri 'none'")
			rewritePortLocation(response, upstream, prefix)
			return nil
		},
		ErrorHandler: func(w http.ResponseWriter, _ *http.Request, _ error) {
			http.Error(w, "host port did not answer", http.StatusBadGateway)
		},
	}
	proxy.ServeHTTP(w, request)
}

// proxyConn cancels the forwarding context when the HTTP transport is done.
type proxyConn struct {
	net.Conn
	cancel context.CancelFunc
}

func (c *proxyConn) Close() error {
	c.cancel()
	return c.Conn.Close()
}

func rewritePortLocation(response *http.Response, upstream *url.URL, prefix string) {
	location, err := response.Location()
	if err != nil {
		return
	}
	if location.Host != "" && location.Host != upstream.Host {
		return
	}
	redirect := &url.URL{
		Path:     prefix + path.Clean("/"+strings.TrimPrefix(location.Path, "/")),
		RawQuery: location.RawQuery, Fragment: location.Fragment,
	}
	response.Header.Set("Location", redirect.String())
}

func (r *Runtime) httpPort(request *http.Request) (string, string, *hostv1.PortServiceDialRequest, error) {
	host, port := request.PathValue("name"), request.PathValue("port")
	n, err := strconv.ParseUint(port, 10, 16)
	if err != nil || n == 0 {
		return "", "", nil, fmt.Errorf("%w: invalid TCP port", access.ErrInvalid)
	}
	if _, err := r.hosts.authorizeHost(request.Context(), host, access.Use, CapabilityPorts); err != nil {
		return "", "", nil, err
	}
	return host, port, &hostv1.PortServiceDialRequest{Target: &hostv1.PortServiceDialRequest_TcpPort{TcpPort: uint32(n)}}, nil
}

func hostHTTPError(w http.ResponseWriter, err error) {
	switch connect.CodeOf(access.ToConnect(err)) {
	case connect.CodeUnauthenticated:
		http.Error(w, "sign in to use this host", http.StatusUnauthorized)
	case connect.CodePermissionDenied:
		http.Error(w, "host access refused", http.StatusForbidden)
	case connect.CodeInvalidArgument:
		http.Error(w, "invalid host request", http.StatusBadRequest)
	case connect.CodeNotFound:
		http.Error(w, "host or terminal not found", http.StatusNotFound)
	case connect.CodeUnavailable:
		http.Error(w, "host unavailable", http.StatusServiceUnavailable)
	default:
		http.Error(w, "host request failed", http.StatusInternalServerError)
	}
}

func closeWebSocket(socket *websocket.Conn, err error) {
	switch {
	case err == nil || errors.Is(err, io.EOF):
		_ = socket.Close(websocket.StatusNormalClosure, "")
	case websocket.CloseStatus(err) != -1:
		_ = socket.CloseNow()
	default:
		_ = socket.Close(websocket.StatusInternalError, "host stream ended")
	}
}
