package cli

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"os/exec"
	"runtime"
	"strings"
	"time"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/authn"
)

// loginTimeout is how long `rookery login` waits for the browser.
const loginTimeout = 5 * time.Minute

// loginCommand is `rookery login`: a session saved for the commands that follow.
func loginCommand() *cobra.Command {
	command := &cobra.Command{
		Use:   "login URL [--token-stdin]",
		Short: "Sign in to a server and save the session",
		Long: `Sign in to a server and save the session.

Without --token-stdin it sends a browser to the server, through a loopback
redirect with PKCE, and saves the access token it comes back with. With
--token-stdin it reads a token from standard input, which is what a script or a
container does.`,
		Args: cobra.ExactArgs(1),
		RunE: func(cmd *cobra.Command, args []string) error {
			fromStdin, err := cmd.Flags().GetBool("token-stdin")
			if err != nil {
				return err
			}
			address := args[0]
			if fromStdin {
				return loginWithToken(cmd, address)
			}
			return loginWithBrowser(cmd, address)
		},
	}
	command.Flags().Bool("token-stdin", false, "read the access token from standard input")
	return command
}

// loginWithToken saves a token read from standard input, once the server says who
// it is.
func loginWithToken(cmd *cobra.Command, address string) error {
	token, err := readSecretLine(cmd.InOrStdin())
	if err != nil {
		return err
	}
	connection, err := dial(cmd.Context(), address, token)
	if err != nil {
		return err
	}
	me, err := connection.Auth.Me(cmd.Context(), connect.NewRequest(&rookeryv1.AuthServiceMeRequest{}))
	if err != nil {
		return err
	}
	if err := saveClientConfig(clientConfig{Server: address, Token: token}); err != nil {
		return err
	}
	_, err = fmt.Fprintf(cmd.OutOrStdout(), "signed in as %s\n", me.Msg.GetUser().GetEmail())
	return err
}

// loginWithBrowser signs in through a browser: the server's /auth/cli page
// redirects to a loopback listener here with a code, and the code and the PKCE
// verifier become a token.
func loginWithBrowser(cmd *cobra.Command, address string) error {
	verifier := authn.NewVerifier()
	state := authn.NewState()

	var listenConfig net.ListenConfig
	listener, err := listenConfig.Listen(cmd.Context(), "tcp", "127.0.0.1:0")
	if err != nil {
		return fmt.Errorf("cli: opening a loopback listener: %w", err)
	}
	defer func() { _ = listener.Close() }()

	port := fmt.Sprint(listener.Addr().(*net.TCPAddr).Port)
	startURL := strings.TrimRight(address, "/") + "/auth/cli?challenge=" + authn.Challenge(verifier) + "&port=" + port + "&state=" + state

	codes := make(chan string, 1)
	server := &http.Server{
		Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			if r.URL.Path != "/callback" {
				http.NotFound(w, r)
				return
			}
			if got := r.URL.Query().Get("state"); got != state {
				http.Error(w, "the state does not match", http.StatusBadRequest)
				return
			}
			w.Header().Set("Content-Type", "text/html; charset=utf-8")
			_, _ = io.WriteString(w, "<!doctype html><title>Rookery</title><p>Signed in. You can close this window.</p>")
			select {
			case codes <- r.URL.Query().Get("code"):
			default:
			}
		}),
		ReadHeaderTimeout: 10 * time.Second,
	}
	go func() {
		if serveErr := server.Serve(listener); serveErr != nil && !errors.Is(serveErr, http.ErrServerClosed) {
			cmd.PrintErrln("cli: the loopback listener stopped:", serveErr)
		}
	}()
	defer func() {
		shutdownCtx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		_ = server.Shutdown(shutdownCtx)
	}()

	_, _ = fmt.Fprintf(cmd.OutOrStdout(), "Open this URL to sign in:\n\n  %s\n\n", startURL)
	if err := openBrowser(cmd.Context(), startURL); err != nil {
		cmd.PrintErrln("cli: no browser could be opened:", err)
	}

	ctx, cancel := context.WithTimeout(cmd.Context(), loginTimeout)
	defer cancel()

	var code string
	select {
	case <-ctx.Done():
		return fmt.Errorf("cli: no browser came back to sign in")
	case code = <-codes:
	}

	token, err := exchangeCode(ctx, address, code, verifier)
	if err != nil {
		return err
	}
	if err := saveClientConfig(clientConfig{Server: address, Token: token}); err != nil {
		return err
	}

	connection, err := dial(cmd.Context(), address, token)
	if err != nil {
		return err
	}
	me, err := connection.Auth.Me(cmd.Context(), connect.NewRequest(&rookeryv1.AuthServiceMeRequest{}))
	if err != nil {
		return err
	}
	_, err = fmt.Fprintf(cmd.OutOrStdout(), "signed in as %s\n", me.Msg.GetUser().GetEmail())
	return err
}

// exchangeCode trades a code and its PKCE verifier for an access token.
func exchangeCode(ctx context.Context, address, code, verifier string) (string, error) {
	body, err := json.Marshal(map[string]string{"code": code, "verifier": verifier})
	if err != nil {
		return "", fmt.Errorf("cli: writing the exchange: %w", err)
	}

	httpClient, base, err := transport(address, "")
	if err != nil {
		return "", err
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, base+"/auth/cli", strings.NewReader(string(body)))
	if err != nil {
		return "", fmt.Errorf("cli: writing the exchange: %w", err)
	}
	request.Header.Set("Content-Type", "application/json")

	response, err := httpClient.Do(request)
	if err != nil {
		return "", fmt.Errorf("cli: exchanging the code: %w", err)
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode != http.StatusOK {
		message, _ := io.ReadAll(io.LimitReader(response.Body, 4096))
		return "", fmt.Errorf("cli: the server refused the exchange: %s", strings.TrimSpace(string(message)))
	}

	var answer cliTokenAnswer
	if err := json.NewDecoder(io.LimitReader(response.Body, 64<<10)).Decode(&answer); err != nil {
		return "", fmt.Errorf("cli: reading the exchange: %w", err)
	}
	if answer.Token == "" {
		return "", fmt.Errorf("cli: the server gave no token")
	}
	return answer.Token, nil
}

// cliTokenAnswer is what the server answers the exchange with.
type cliTokenAnswer struct {
	Token  string `json:"token"`
	Server string `json:"server"`
}

// logoutCommand is `rookery logout`: forget the saved session.
func logoutCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "logout",
		Short: "Forget the saved session",
		RunE: func(cmd *cobra.Command, _ []string) error {
			if err := clearClientConfig(); err != nil {
				return err
			}
			_, err := fmt.Fprintln(cmd.OutOrStdout(), "signed out")
			return err
		},
	}
}

// whoamiCommand is `rookery whoami`: who the saved session is.
func whoamiCommand() *cobra.Command {
	return &cobra.Command{
		Use:   "whoami",
		Short: "Show who the session is",
		RunE: func(cmd *cobra.Command, _ []string) error {
			session, err := open(cmd)
			if err != nil {
				return err
			}
			me, err := session.api.Auth.Me(cmd.Context(), connect.NewRequest(&rookeryv1.AuthServiceMeRequest{}))
			if err != nil {
				return err
			}
			user := me.Msg.GetUser()
			role := "user"
			if user.GetAdmin() {
				role = "admin"
			}
			return session.printer.one(user, []string{"EMAIL", "NAME", "ROLE"}, []string{user.GetEmail(), user.GetName(), role})
		},
	}
}

// readSecretLine reads a secret from standard input: one line, without its
// newline, and never through argv.
func readSecretLine(in io.Reader) (string, error) {
	line, err := bufio.NewReader(in).ReadString('\n')
	if err != nil && !errors.Is(err, io.EOF) {
		return "", fmt.Errorf("cli: reading the token: %w", err)
	}
	secret := strings.TrimSpace(line)
	if secret == "" {
		return "", fmt.Errorf("cli: no token on standard input")
	}
	return secret, nil
}

// openBrowser opens a URL in the machine's browser, and says so when it cannot.
func openBrowser(ctx context.Context, url string) error {
	var command *exec.Cmd
	switch runtime.GOOS {
	case "darwin":
		command = exec.CommandContext(ctx, "open", url) //nolint:gosec // the operator's URL
	case "windows":
		command = exec.CommandContext(ctx, "rundll32", "url.dll,FileProtocolHandler", url) //nolint:gosec
	default:
		command = exec.CommandContext(ctx, "xdg-open", url) //nolint:gosec
	}
	if err := command.Start(); err != nil {
		return err
	}
	return nil
}
