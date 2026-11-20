package hosts

import (
	"context"
	"errors"
	"fmt"
	"log/slog"
	"sort"
	"time"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/messaging"
	"github.com/aiviarium/rookery/internal/profile"
)

// EventHostsChanged is published when a host is added, changed or removed.
const EventHostsChanged = "rookery/hosts.changed"

// Deps is what the host service is built from.
type Deps struct {
	// ValidateFeatures checks plugin declarations before saving a profile.
	ValidateFeatures func(context.Context, profile.Profile) error
	Hosts            *Hosts
	Checker          *access.Checker
	// Jobs enqueues first contact; a host service without one cannot add a host
	// with a credential.
	Jobs *jobs.Service
	// Links reaches the hosts.
	Links *link.Manager
	// Bus carries the host events a Watch streams.
	Bus *messaging.Bus
	// Versions is the versions of the installed plugins, which the digest is
	// taken over.
	Versions func(ctx context.Context) (map[string]string, error)
	// Version is the server's own version.
	Version string
	// Log is the logger.
	Log *slog.Logger
}

// Service is HostService.
type Service struct {
	deps Deps
	// credentials holds a first contact's password or key until the job that
	// uses it runs: it is never stored and never logged.
	credentials *Credentials
	operations  *Operations
}

// NewService returns the host service.
func NewService(deps Deps) *Service {
	if deps.Versions == nil {
		deps.Versions = func(context.Context) (map[string]string, error) { return nil, nil }
	}
	return &Service{deps: deps, credentials: NewCredentials()}
}

// SetJobs gives the service the job runner, which every host has: first contact
// is a job.
func (s *Service) SetJobs(runner *jobs.Service) { s.deps.Jobs = runner }

// SetLinks gives the service the link manager, which first contact and every
// later call to the host use.
func (s *Service) SetLinks(manager *link.Manager) { s.deps.Links = manager }

// ContactHandler is the host.contact job: first contact with a host.
func (s *Service) ContactHandler() jobs.Handler {
	return jobs.HandlerFunc(func(ctx context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
		var args ContactArgs
		if err := decode(job.Args, &args); err != nil {
			return nil, err
		}
		if args.Host != job.Host || job.Host == "" {
			return nil, fmt.Errorf("%w: contact host must match the job resource", access.ErrInvalid)
		}
		credential := s.credentials.Take(job.ID)
		target, err := s.Target(ctx, job.Host)
		if err != nil {
			return nil, err
		}
		_ = log.WriteLine(fmt.Sprintf("contacting %s at %s", target.Name, target.Address)) //nolint:contextcheck // the log holds the job's context
		contact, err := s.deps.Links.Contact(ctx, target, credential, s.deps.Log)
		if err != nil {
			return nil, err
		}
		if err := s.deps.Hosts.Touch(ctx, args.Host); err != nil {
			return nil, err
		}
		if contact.Fingerprint != "" {
			_ = log.WriteLine(fmt.Sprintf("the host key is %s", contact.Fingerprint)) //nolint:contextcheck // the log holds the job's context
		}
		_ = log.WriteLine(contact.Message) //nolint:contextcheck // the log holds the job's context
		s.publish(ctx, args.Host, "updated")
		return encode(contact)
	})
}

// Target is where a host is, for a link.
func (s *Service) Target(ctx context.Context, name string) (link.Target, error) {
	record, err := s.deps.Hosts.Get(ctx, name)
	if err != nil {
		return link.Target{}, err
	}
	return targetOf(record), nil
}

func targetOf(record Record) link.Target {
	target := link.Target{
		Name:    record.Name,
		Address: record.Profile.Address,
		Port:    record.Profile.Port,
		SSHUser: record.Profile.SSHUser,
	}
	if record.Address != "" {
		target.Address = record.Address
		target.TailnetAddress = record.Address
	}
	return target
}

// Target looks a host up for the link manager.
func (s *Service) targetFunc() link.Targets { return link.TargetsFunc(s.Target) }

// Targets is the lookup the link manager is wired with.
func (s *Service) Targets() link.Targets { return s.targetFunc() }

// List returns the hosts the caller may view.
func (s *Service) List(ctx context.Context, request *connect.Request[rookeryv1.HostServiceListRequest]) (*connect.Response[rookeryv1.HostServiceListResponse], error) {
	limit := int(request.Msg.GetLimit())
	if limit <= 0 {
		limit = 100
	}
	records, err := s.deps.Hosts.List(ctx, Filter{
		Labels: request.Msg.GetLabel(),
		After:  request.Msg.GetCursor(),
		Limit:  limit,
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}
	principal := access.MustPrincipal(ctx)
	response := &rookeryv1.HostServiceListResponse{}
	var last string
	for _, record := range records {
		level, err := s.deps.Checker.Level(ctx, principal, record.URI())
		if err != nil {
			return nil, access.ToConnect(err)
		}
		if !access.Satisfies(level, access.View) {
			continue
		}
		host, err := s.message(ctx, record)
		if err != nil {
			return nil, access.ToConnect(err)
		}
		response.Hosts = append(response.Hosts, host)
		last = record.Name
	}
	if len(records) == limit {
		response.NextCursor = last
	}
	return connect.NewResponse(response), nil
}

// Get returns one host.
func (s *Service) Get(ctx context.Context, request *connect.Request[rookeryv1.HostServiceGetRequest]) (*connect.Response[rookeryv1.HostServiceGetResponse], error) {
	record, err := s.deps.Hosts.Get(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	host, err := s.message(ctx, record)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceGetResponse{Host: host}), nil
}

// Watch streams each host change the caller may view, starting with the hosts
// as they are now.
func (s *Service) Watch(ctx context.Context, _ *connect.Request[rookeryv1.HostServiceWatchRequest], stream *connect.ServerStream[rookeryv1.HostServiceWatchResponse]) error {
	subscription := s.deps.Bus.Subscribe(EventHostsChanged, nil)
	defer subscription.Close()

	for {
		select {
		case <-ctx.Done():
			return nil
		case event, ok := <-subscription.Events:
			if !ok {
				return nil
			}
			name := eventPayloadName(event.Payload)
			if name == "" {
				continue
			}
			record, err := s.deps.Hosts.Get(ctx, name)
			if err != nil {
				continue
			}
			level, err := s.deps.Checker.Level(ctx, access.MustPrincipal(ctx), record.URI())
			if err != nil || !access.Satisfies(level, access.View) {
				continue
			}
			host, err := s.message(ctx, record)
			if err != nil {
				continue
			}
			if err := stream.Send(&rookeryv1.HostServiceWatchResponse{Change: eventPayloadChange(event.Payload), Hosts: []*rookeryv1.Host{host}}); err != nil {
				return err
			}
		}
	}
}

// Add records a host and, with a credential, starts first contact as a job.
func (s *Service) Add(ctx context.Context, request *connect.Request[rookeryv1.HostServiceAddRequest]) (*connect.Response[rookeryv1.HostServiceAddResponse], error) {
	principal, ok := access.PrincipalFrom(ctx)
	if !ok {
		return nil, access.ToConnect(access.ErrUnauthenticated)
	}
	hostType := fromProtoType(request.Msg.GetType())
	p := profile.Profile{
		Type:    hostType,
		Address: request.Msg.GetAddress(),
		Port:    int(request.Msg.GetPort()),
		SSHUser: request.Msg.GetUser(),
		Tailnet: true,
	}

	if hostType == profile.TypeWorkstation {
		p.Desktop = &profile.Desktop{}
	}
	record, err := s.AddHost(ctx, AddInput{
		Name:      request.Msg.GetName(),
		Type:      hostType,
		Profile:   p,
		CreatedBy: principal.ID,
	})
	if err != nil {
		return nil, access.ToConnect(err)
	}

	response := &rookeryv1.HostServiceAddResponse{}
	host, err := s.message(ctx, record)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	response.Host = host

	credential := credentialOf(request.Msg.GetCredential())
	if !credential.Empty() {
		job, err := s.enqueueContact(ctx, record.Name, credential)
		if err != nil {
			return nil, access.ToConnect(err)
		}
		response.FirstContact = job.ID
	}
	return connect.NewResponse(response), nil
}

// AddHost records a host, grants its creator manage, and publishes it. A
// provisioner that makes a machine calls it too.
func (s *Service) AddHost(ctx context.Context, input AddInput) (Record, error) {
	record, err := s.deps.Hosts.Add(ctx, input)
	if err != nil {
		return Record{}, err
	}
	// The creator manages the host.
	if input.CreatedBy != "" {
		if err := s.deps.Checker.GrantUser(ctx, input.CreatedBy, record.URI(), access.Manage); err != nil {
			return Record{}, err
		}
	}
	s.publish(ctx, record.Name, "added")
	return record, nil
}

// Remove deletes a host.
func (s *Service) Remove(ctx context.Context, request *connect.Request[rookeryv1.HostServiceRemoveRequest]) (*connect.Response[rookeryv1.HostServiceRemoveResponse], error) {
	if err := s.RemoveHost(ctx, request.Msg.GetName()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceRemoveResponse{}), nil
}

// RemoveHost deletes a host and its grants, and tells watchers it went. A
// provisioner that removes a machine calls it too.
func (s *Service) RemoveHost(ctx context.Context, name string) error {
	if err := s.deps.Hosts.Remove(ctx, name); err != nil {
		return err
	}
	s.disconnect(name)
	s.publish(ctx, name, "removed")
	return nil
}

// Rename renames a host and keeps its grants.
func (s *Service) Rename(ctx context.Context, request *connect.Request[rookeryv1.HostServiceRenameRequest]) (*connect.Response[rookeryv1.HostServiceRenameResponse], error) {
	record, err := s.deps.Hosts.Rename(ctx, request.Msg.GetName(), request.Msg.GetNewName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	s.disconnect(request.Msg.GetName())
	s.publish(ctx, request.Msg.GetName(), "removed")
	s.publish(ctx, record.Name, "added")
	host, err := s.message(ctx, record)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceRenameResponse{Host: host}), nil
}

// SetProfile stores a host's profile.
func (s *Service) SetProfile(ctx context.Context, request *connect.Request[rookeryv1.HostServiceSetProfileRequest]) (*connect.Response[rookeryv1.HostServiceSetProfileResponse], error) {
	p, err := fromProtoProfile(request.Msg.GetProfile())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	if s.deps.ValidateFeatures != nil {
		if err := s.deps.ValidateFeatures(ctx, p); err != nil {
			return nil, access.ToConnect(err)
		}
	}
	current, err := s.deps.Hosts.Get(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	normalized := p.Normalize()
	if normalized.Address != current.Profile.Address || normalized.Port != current.Profile.Port || normalized.SSHUser != current.Profile.SSHUser {
		// Routing a host to another SSH address can reuse the deployment
		// key there. Only an admin may change what a host resource identifies.
		principal, _ := access.PrincipalFrom(ctx)
		if err := s.deps.Checker.Check(ctx, principal, "", access.Admin); err != nil {
			return nil, access.ToConnect(err)
		}
	}
	record, err := s.deps.Hosts.SetProfile(ctx, request.Msg.GetName(), p)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	s.disconnect(record.Name)
	s.publish(ctx, record.Name, "updated")
	host, err := s.message(ctx, record)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceSetProfileResponse{Host: host}), nil
}

// SetLabels puts and takes a host's labels.
func (s *Service) SetLabels(ctx context.Context, request *connect.Request[rookeryv1.HostServiceSetLabelsRequest]) (*connect.Response[rookeryv1.HostServiceSetLabelsResponse], error) {
	if err := s.deps.Hosts.SetLabels(ctx, request.Msg.GetName(), request.Msg.GetSet(), request.Msg.GetRemove()); err != nil {
		return nil, access.ToConnect(err)
	}
	record, err := s.deps.Hosts.Get(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	s.publish(ctx, record.Name, "updated")
	host, err := s.message(ctx, record)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.HostServiceSetLabelsResponse{Host: host}), nil
}

// Check answers whether a host can do a capability, and says what would change
// the answer when it cannot.
func (s *Service) Check(ctx context.Context, request *connect.Request[rookeryv1.HostServiceCheckRequest]) (*connect.Response[rookeryv1.HostServiceCheckResponse], error) {
	record, err := s.deps.Hosts.Get(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	capability := fromProtoCapability(request.Msg.GetCapability())
	response := &rookeryv1.HostServiceCheckResponse{Type: toProtoType(record.Type)}
	if err := Check(record, capability); err != nil {
		var denied *access.Denied
		if errors.As(err, &denied) {
			response.Reason = denied.Reason
		} else {
			response.Reason = err.Error()
		}
		return connect.NewResponse(response), nil
	}
	response.Allowed = true
	return connect.NewResponse(response), nil
}

// message is a host as the API has it.
func (s *Service) message(ctx context.Context, record Record) (*rookeryv1.Host, error) {
	host := &rookeryv1.Host{
		Name:          record.Name,
		Type:          toProtoType(record.Type),
		Profile:       toProtoProfile(record.Profile),
		Address:       record.Address,
		AppliedDigest: record.AppliedDigest,
		Labels:        record.Labels,
		CreatedBy:     record.CreatedBy,
		CreatedAt:     timestamp(record.CreatedAt),
		UpdatedAt:     timestamp(record.UpdatedAt),
		AppliedAt:     timestampPtr(record.AppliedAt),
		LastSeenAt:    timestampPtr(record.LastSeenAt),
	}
	versions, err := s.deps.Versions(ctx)
	if err != nil {
		return nil, err
	}
	digest, err := profile.Digest(record.Profile, versions, s.deps.Version)
	if err != nil {
		// A profile whose features name no plugin installed still has a digest;
		// one that does not validate is a host that cannot be applied, and the
		// digest is left empty rather than failing the read.
		return host, nil //nolint:nilerr // the host is still readable
	}
	host.Digest = digest
	return host, nil
}

// enqueueContact starts first contact for a host.
func (s *Service) enqueueContact(ctx context.Context, name string, credential link.Credential) (jobs.Job, error) {
	if s.deps.Jobs == nil {
		return jobs.Job{}, fmt.Errorf("%w: first contact needs the job runner", access.ErrUnavailable)
	}
	args, err := encode(ContactArgs{Host: name, Credential: true})
	if err != nil {
		return jobs.Job{}, err
	}
	// A worker may claim the job immediately after the database commit.
	// Hold the same lock Take uses until its credential is available.
	s.credentials.mu.Lock()
	defer s.credentials.mu.Unlock()
	job, err := s.deps.Jobs.Enqueue(ctx, jobs.EnqueueRequest{
		Kind:     "host.contact",
		Args:     args,
		Resource: access.HostURI(name),
	})
	if err == nil {
		s.credentials.byID[job.ID] = credential
	}
	return job, err
}

func (s *Service) disconnect(name string) {
	if s.deps.Links != nil {
		s.deps.Links.Disconnect(name)
	}
}

// publish sends a host event, which a Watch turns into a stream message and a
// client half into a refresh.
func (s *Service) publish(_ context.Context, name, change string) {
	if s.deps.Bus == nil {
		return
	}
	payload, err := encode(hostEvent{Name: name, Change: change})
	if err != nil {
		return
	}
	s.deps.Bus.Publish(messaging.Event{Name: EventHostsChanged, Resource: access.HostURI(name), Payload: payload})
}

// hostEvent is a change to one host.
type hostEvent struct {
	Name   string `cbor:"name"`
	Change string `cbor:"change"`
}

// eventPayloadName reads the host a change is about.
func eventPayloadName(payload []byte) string {
	var event hostEvent
	if err := decode(payload, &event); err != nil {
		return ""
	}
	return event.Name
}

// eventPayloadChange reads what happened to a host.
func eventPayloadChange(payload []byte) string {
	var event hostEvent
	if err := decode(payload, &event); err != nil {
		return ""
	}
	return event.Change
}

// ContactArgs is a host.contact job's arguments.
type ContactArgs struct {
	Host string `cbor:"host"`
	// Credential says a credential is waiting in memory for this job.
	Credential bool `cbor:"credential,omitempty"`
}

// Labels is a host's labels, sorted, for a table.
func Labels(labels map[string]string) []string {
	keys := make([]string, 0, len(labels))
	for key := range labels {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	out := make([]string, 0, len(keys))
	for _, key := range keys {
		out = append(out, key+"="+labels[key])
	}
	return out
}

// timestamp is a proto timestamp, or nil for zero.
func timestamp(at time.Time) *timestamppb.Timestamp {
	if at.IsZero() {
		return nil
	}
	return timestamppb.New(at)
}

// timestampPtr is a proto timestamp for an optional time.
func timestampPtr(at *time.Time) *timestamppb.Timestamp {
	if at == nil || at.IsZero() {
		return nil
	}
	return timestamppb.New(*at)
}
