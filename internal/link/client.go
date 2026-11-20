package link

import (
	"net/http"

	"connectrpc.com/connect"

	hostv1connect "github.com/aiviarium/rookery/gen/go/rookery/host/v1/hostv1connect"
)

// Client is a host's daemon as a link reaches it: connect clients over the SSH
// session's pipe, speaking gRPC in cleartext HTTP/2.
//
// It is the link's own client rather than agent's, so that a `rookery` built for
// a machine that is not a host does not carry the daemon's code: the daemon is
// Linux-only, the CLI is not.
type Client struct {
	http *http.Client

	Agent     hostv1connect.AgentServiceClient
	Sessions  hostv1connect.SessionServiceClient
	Exec      hostv1connect.ExecServiceClient
	Files     hostv1connect.FileServiceClient
	Ports     hostv1connect.PortServiceClient
	Channel   hostv1connect.ChannelServiceClient
	HostCalls hostv1connect.HostCallServiceClient
	Actions   hostv1connect.ActionServiceClient
	Desktop   hostv1connect.DesktopServiceClient
}

// Close ends the client's idle streams. The pipe itself belongs to the link.
func (c *Client) Close() error {
	c.http.CloseIdleConnections()
	return nil
}

// clients builds the daemon's clients on an HTTP client.
func clients(httpClient *http.Client) *Client {
	options := []connect.ClientOption{connect.WithGRPC()}
	const baseURL = "http://rookeryd"
	return &Client{
		http:      httpClient,
		Agent:     hostv1connect.NewAgentServiceClient(httpClient, baseURL, options...),
		Sessions:  hostv1connect.NewSessionServiceClient(httpClient, baseURL, options...),
		Exec:      hostv1connect.NewExecServiceClient(httpClient, baseURL, options...),
		Files:     hostv1connect.NewFileServiceClient(httpClient, baseURL, options...),
		Ports:     hostv1connect.NewPortServiceClient(httpClient, baseURL, options...),
		Channel:   hostv1connect.NewChannelServiceClient(httpClient, baseURL, options...),
		HostCalls: hostv1connect.NewHostCallServiceClient(httpClient, baseURL, options...),
		Actions:   hostv1connect.NewActionServiceClient(httpClient, baseURL, options...),
		Desktop:   hostv1connect.NewDesktopServiceClient(httpClient, baseURL, options...),
	}
}
