package hosts

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"connectrpc.com/connect"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/apply"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/jobs"
	"github.com/aiviarium/rookery/internal/link"
	"github.com/aiviarium/rookery/internal/profile"
	"github.com/aiviarium/rookery/internal/tailnet"
)

func (o *Operations) apply(ctx context.Context, job jobs.Job, log *jobs.Log) ([]byte, error) {
	if job.Host == "" {
		return nil, fmt.Errorf("%w: an apply needs a host resource", access.ErrInvalid)
	}
	if o.options.Apply == nil {
		return nil, fmt.Errorf("%w: no apply runner is configured", access.ErrUnavailable)
	}

	service := o.runtime.hosts
	record, err := service.deps.Hosts.Get(ctx, job.Host)
	if err != nil {
		return nil, err
	}
	if err := Check(record, CapabilityApply); err != nil {
		return nil, err
	}
	desired := Grow(profile.TypeManaged, record.Profile.Type)
	resolved, err := o.resolve(ctx, record.Profile, desired)
	if err != nil {
		return nil, err
	}

	request, err := o.prepareApply(ctx, record, resolved, job.ID, log)
	if err != nil {
		return nil, err
	}
	defer clear(request.Key.PrivateKey)
	result, err := o.options.Apply.Apply(ctx, request)
	if err != nil {
		return nil, err
	}

	service.deps.Links.Disconnect(record.Name)
	version, err := o.waitForAgent(ctx, record.Name, desired == profile.TypeWorkstation)
	if err != nil {
		return nil, err
	}
	actual := profile.TypeManaged
	if version.GetDesktop().GetAvailable() {
		actual = profile.TypeWorkstation
	}
	address, err := o.appliedAddress(ctx, record)
	if err != nil {
		return nil, err
	}
	recorded, err := service.deps.Hosts.MarkApplied(ctx, record.Name, result.Digest, actual, address)
	if err != nil {
		return nil, err
	}
	if address != "" && address != record.Address {
		service.deps.Links.Disconnect(record.Name)
	}
	service.publish(ctx, record.Name, "updated")
	return cborx.Marshal(map[string]string{"digest": result.Digest, "version": version.GetVersion(), "type": string(recorded.Type)})
}

func (o *Operations) prepareApply(ctx context.Context, record Record, resolved featureSet, jobID string, log *jobs.Log) (apply.Request, error) {
	registries, err := o.registries(ctx, record.Profile.Registries)
	if err != nil {
		return apply.Request{}, err
	}
	features, err := o.applyFeatures(ctx, resolved)
	if err != nil {
		return apply.Request{}, err
	}
	service := o.runtime.hosts
	links := service.deps.Links
	key, err := links.Ring().Active(ctx)
	if err != nil {
		return apply.Request{}, err
	}
	target := targetOf(record)
	arch, err := probeArchitecture(ctx, links, target, key)
	if err != nil {
		return apply.Request{}, err
	}
	binaries, err := releaseBinaries(o.options.ReleaseDir, arch)
	if err != nil {
		return apply.Request{}, err
	}
	knownKey, err := links.KnownHosts().Line(ctx, link.Address(target.Address, target.Port))
	if err != nil {
		return apply.Request{}, err
	}
	authKey, err := o.tailnetKey(ctx, record)
	if err != nil {
		return apply.Request{}, err
	}
	private, err := links.Ring().PrivatePEM(key)
	if err != nil {
		return apply.Request{}, err
	}
	actions := make([]apply.Action, 0, len(resolved.actions))
	for _, action := range resolved.actions {
		if action.OnHost {
			actions = append(actions, action)
		}
	}
	return apply.Request{
		JobID: jobID, Host: apply.Host{Name: record.Name, TailnetAddress: target.TailnetAddress},
		Profile: record.Profile, Plugins: resolved.versions, Rookery: service.deps.Version,
		Key:      apply.KeyMaterial{PrivateKey: private, KnownHost: knownKey},
		Features: features, Registries: registries, Actions: actions,
		AuthKey: authKey, LoginServer: o.options.TailnetControl, Binaries: binaries, Sink: applyLog{log},
	}, nil
}

func probeArchitecture(ctx context.Context, links *link.Manager, target link.Target, key *link.Key) (string, error) {
	probe, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	var architecture bytes.Buffer
	exit, err := links.RunSSH(probe, target, link.Command{
		Program: "uname", Args: []string{"-s", "-m"}, Stdout: &architecture, Stderr: io.Discard, Key: key,
	})
	if err != nil {
		return "", err
	}
	if exit != 0 {
		return "", fmt.Errorf("%w: reading the host architecture exited with status %d", access.ErrUnavailable, exit)
	}
	switch machine := strings.Fields(architecture.String()); {
	case len(machine) != 2 || machine[0] != "Linux":
		return "", fmt.Errorf("%w: apply needs a Linux host", access.ErrUnavailable)
	case machine[1] == "x86_64":
		return "amd64", nil
	case machine[1] == "aarch64":
		return "arm64", nil
	default:
		return "", fmt.Errorf("%w: apply does not support %s", access.ErrUnavailable, machine[1])
	}
}

func releaseBinaries(dir, arch string) (apply.Binaries, error) {
	if dir == "" {
		executable, err := os.Executable()
		if err != nil {
			return apply.Binaries{}, fmt.Errorf("hosts: locating Rookery binaries: %w", err)
		}
		dir = filepath.Dir(executable)
	}
	dir, err := filepath.Abs(dir)
	if err != nil {
		return apply.Binaries{}, err
	}
	paths := make(map[string]string, 2)
	for _, name := range []string{"rookeryd", "box"} {
		candidates := []string{filepath.Join(dir, name+"-linux-"+arch)}
		if runtime.GOOS == "linux" && arch == runtime.GOARCH {
			candidates = append(candidates, filepath.Join(dir, name))
		}
		for _, path := range candidates {
			info, err := os.Stat(path)
			if err == nil && info.Mode().IsRegular() {
				paths[name] = path
				break
			}
			if err != nil && !errors.Is(err, os.ErrNotExist) {
				return apply.Binaries{}, err
			}
		}
		if paths[name] == "" {
			return apply.Binaries{}, fmt.Errorf("%w: no linux/%s %s in %s", access.ErrUnavailable, arch, name, dir)
		}
	}
	return apply.Binaries{Rookeryd: paths["rookeryd"], Box: paths["box"]}, nil
}

func (o *Operations) waitForAgent(ctx context.Context, host string, desktop bool) (*hostv1.AgentServiceVersionResponse, error) {
	ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
	defer cancel()
	ticker := time.NewTicker(250 * time.Millisecond)
	defer ticker.Stop()
	links := o.runtime.hosts.deps.Links
	for {
		client, release, err := links.Use(ctx, host)
		if err == nil {
			response, callErr := client.Agent.Version(ctx, connect.NewRequest(&hostv1.AgentServiceVersionRequest{}))
			release()
			if callErr == nil && (!desktop || response.Msg.GetDesktop().GetAvailable()) {
				return response.Msg, nil
			}
			err = callErr
			if err == nil {
				err = errors.New("desktop provider unavailable")
			}
		}
		select {
		case <-ctx.Done():
			return nil, fmt.Errorf("hosts: verifying the applied host: %w", errors.Join(ctx.Err(), err))
		case <-ticker.C:
		}
	}
}

func (o *Operations) tailnetKey(ctx context.Context, host Record) (string, error) {
	if !host.Profile.Tailnet || o.options.Tailnet == nil {
		return "", nil
	}
	key, err := o.options.Tailnet.MintKey(ctx, tailnet.KeyRequest{Hostname: host.Name, Tags: o.options.TailnetTags})
	if err != nil {
		return "", fmt.Errorf("hosts: minting a tailnet key for %s: %w", host.Name, err)
	}
	return key.Secret, nil
}

func (o *Operations) appliedAddress(ctx context.Context, host Record) (string, error) {
	if !host.Profile.Tailnet || o.options.Tailnet == nil {
		return host.Address, nil
	}
	device, err := o.options.Tailnet.Device(ctx, host.Name)
	if errors.Is(err, tailnet.ErrNotFound) {
		return host.Address, nil
	}
	if err != nil {
		return "", fmt.Errorf("hosts: looking up the tailnet address for %s: %w", host.Name, err)
	}
	if !device.Tagged(o.options.TailnetTags) {
		return host.Address, nil
	}
	return device.Address(), nil
}

type applyLog struct{ job *jobs.Log }

func (l applyLog) Line(ctx context.Context, line string) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	return l.job.WriteLine(line) //nolint:contextcheck // the log owns the job's context
}
