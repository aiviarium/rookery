package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"net/http"
	"net/url"
	"os"
	"os/signal"
	"strings"
	"sync"
	"syscall"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"

	examplev1 "github.com/aiviarium/rookery/gen/go/example/v1"
	"github.com/aiviarium/rookery/gen/go/example/v1/examplev1connect"
	pluginv1 "github.com/aiviarium/rookery/gen/go/rookery/plugin/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
	"github.com/aiviarium/rookery/pkg/sdk"
)

type service struct{ client *sdk.Client }

func (s service) Summarize(ctx context.Context, request *connect.Request[examplev1.SummarizeRequest]) (*connect.Response[examplev1.SummarizeResponse], error) {
	codec := s.client.AsCaller(request.Header().Get(sdk.CallerTokenHeader))
	auth := sdk.CoreClient(codec, rookeryv1connect.NewAuthServiceClient)
	me, err := auth.Me(ctx, connect.NewRequest(&rookeryv1.AuthServiceMeRequest{}))
	if err != nil {
		return nil, err
	}
	summary := fmt.Sprintf("%d note(s): %s", len(request.Msg.GetTitles()), strings.Join(request.Msg.GetTitles(), ", "))
	if err := s.record(ctx, "summaries"); err != nil {
		return nil, err
	}
	payload, err := sdk.Marshal(summary)
	if err != nil {
		return nil, err
	}
	_, err = s.client.Server().Publish(ctx, connect.NewRequest(&pluginv1.ServerServicePublishRequest{
		Name: "example/note.changed", PayloadCbor: payload,
	}))
	if err != nil {
		return nil, err
	}
	return connect.NewResponse(&examplev1.SummarizeResponse{Summary: summary, User: me.Msg.GetUser().GetEmail()}), nil
}

func (s service) record(ctx context.Context, key string) error {
	server := s.client.Server()
	for {
		old, err := server.GetKV(ctx, connect.NewRequest(&pluginv1.ServerServiceGetKVRequest{Key: key}))
		var count, revision int64
		switch connect.CodeOf(err) {
		case connect.CodeNotFound:
		default:
			if err != nil {
				return err
			}
			revision = old.Msg.GetRevision()
			if err := sdk.Unmarshal(old.Msg.GetValueCbor(), &count); err != nil {
				return err
			}
		}
		next, err := sdk.Marshal(count + 1)
		if err != nil {
			return err
		}
		_, err = server.SetKV(ctx, connect.NewRequest(&pluginv1.ServerServiceSetKVRequest{Key: key, ExpectedRevision: &revision, ValueCbor: next}))
		if connect.CodeOf(err) != connect.CodeAborted {
			return err
		}
	}
}

func (s service) reindex(ctx context.Context, work sdk.Work) (any, error) {
	if err := work.Line(ctx, "reindexing notes"); err != nil {
		return nil, err
	}
	server := s.client.Server()
	credentials, err := server.DatabaseCredentials(ctx, connect.NewRequest(&pluginv1.ServerServiceDatabaseCredentialsRequest{}))
	if err != nil {
		return nil, err
	}
	db, err := pgx.Connect(ctx, credentials.Msg.GetDsn())
	if err != nil {
		return nil, errors.New("could not connect to the example SQL schema")
	}
	defer func() { _ = db.Close(ctx) }()
	if _, err := db.Exec(ctx, "CREATE TABLE IF NOT EXISTS p_example_sql.reindex_runs (ran_at timestamptz DEFAULT now())"); err != nil {
		return nil, err
	}
	if _, err := db.Exec(ctx, "INSERT INTO p_example_sql.reindex_runs DEFAULT VALUES"); err != nil {
		return nil, err
	}
	if err := s.record(ctx, "reindex_runs"); err != nil {
		return nil, err
	}
	if user, ok := strings.CutPrefix(work.Job.GetCreatedBy(), "user:"); ok {
		_, err := server.SendNotification(ctx, connect.NewRequest(&pluginv1.ServerServiceSendNotificationRequest{
			UserId: user, Title: "Notes reindexed", Body: "The notes index is up to date.",
		}))
		if err != nil {
			return nil, err
		}
	}
	return "reindexed", nil
}

func testConnection(_ context.Context, kind string, data []byte) (string, error) {
	var values map[string]string
	if err := sdk.Unmarshal(data, &values); err != nil {
		return "", errors.New("invalid notes connection")
	}
	endpoint, err := url.Parse(values["url"])
	if kind != "notes-api" || err != nil || endpoint.Scheme != "https" || endpoint.Host == "" || values["token"] == "" {
		return "", errors.New("the notes connection needs an HTTPS URL and a token")
	}
	return "Notes connection is configured.", nil
}

func run(ctx context.Context) error {
	config, err := sdk.ConfigFromEnv()
	if err != nil {
		return err
	}
	client, err := sdk.NewClient(config)
	if err != nil {
		return err
	}
	service := service{client: client}
	ctx, cancel := context.WithCancel(ctx)
	defer cancel()
	initialized := make(chan struct{})
	var once sync.Once
	worker := make(chan error, 1)
	go func() {
		select {
		case <-ctx.Done():
			worker <- nil
		case <-initialized:
			worker <- client.RunWorker(ctx, []string{"reindex"}, service.reindex)
			cancel()
		}
	}()
	err = sdk.ServePlugin(ctx, config.PluginSocket, sdk.Service{
		OnInit: func(context.Context, *pluginv1.PluginServiceInitRequest) error {
			once.Do(func() { close(initialized) })
			return nil
		},
		OnTestConnection: testConnection,
	}, func(mux *http.ServeMux) {
		path, handler := examplev1connect.NewNoteServiceHandler(service)
		mux.Handle(path, handler)
	})
	cancel()
	workerErr := <-worker
	if ctx.Err() != nil && connect.CodeOf(workerErr) == connect.CodeCanceled {
		workerErr = nil
	}
	return errors.Join(err, workerErr)
}

func main() {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	if err := run(ctx); err != nil {
		log.Fatal(err)
	}
}
