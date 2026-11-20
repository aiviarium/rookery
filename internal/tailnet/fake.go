package tailnet

import (
	"context"
	"fmt"
	"net"
	"slices"
	"sync"
	"time"
)

// FakeControl is an in-memory Control for tests: it mints keys, keeps the
// devices a test adds, and remembers what it was asked to do.
type FakeControl struct {
	mutex sync.Mutex
	// keys are the minted keys' requests, in the order they were minted.
	keys []KeyRequest
	// minted are the keys themselves, by secret.
	minted map[string]Key
	// devices are the devices on the tailnet, by id.
	devices map[string]Device
	// next numbers the keys and the devices so that no two are alike.
	next int
	// Err, when set, is what every call answers with.
	Err error
}

// NewFakeControl makes an empty tailnet.
func NewFakeControl() *FakeControl {
	return &FakeControl{minted: map[string]Key{}, devices: map[string]Device{}}
}

// MintKey makes a key that looks like a control server's, and records what it
// was asked for.
func (f *FakeControl) MintKey(_ context.Context, request KeyRequest) (Key, error) {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	if f.Err != nil {
		return Key{}, f.Err
	}
	f.next++
	key := Key{
		ID:      fmt.Sprintf("key-%d", f.next),
		Secret:  fmt.Sprintf("tskey-fake-%d-%s", f.next, request.Hostname),
		Expires: time.Now().Add(DefaultKeyExpiry),
	}
	f.keys = append(f.keys, request)
	f.minted[key.Secret] = key
	return key, nil
}

// Device looks a device up by its hostname.
func (f *FakeControl) Device(_ context.Context, hostname string) (Device, error) {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	if f.Err != nil {
		return Device{}, f.Err
	}
	for _, device := range f.devices {
		if device.Hostname == hostname {
			return device, nil
		}
	}
	return Device{}, fmt.Errorf("%w: no device named %q", ErrNotFound, hostname)
}

// DeleteDevice removes a device.
func (f *FakeControl) DeleteDevice(_ context.Context, id string) error {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	if f.Err != nil {
		return f.Err
	}
	if _, ok := f.devices[id]; !ok {
		return fmt.Errorf("%w: device %q", ErrNotFound, id)
	}
	delete(f.devices, id)
	return nil
}

// Devices lists the tagged devices.
func (f *FakeControl) Devices(_ context.Context) ([]Device, error) {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	if f.Err != nil {
		return nil, f.Err
	}
	tagged := make([]Device, 0, len(f.devices))
	for _, device := range f.devices {
		if len(device.Tags) > 0 {
			tagged = append(tagged, device)
		}
	}
	slices.SortFunc(tagged, func(a, b Device) int {
		switch {
		case a.Hostname < b.Hostname:
			return -1
		case a.Hostname > b.Hostname:
			return 1
		}
		return 0
	})
	return tagged, nil
}

// Join adds a device, as a host that used a key does.
func (f *FakeControl) Join(device Device) Device {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	if device.ID == "" {
		f.next++
		device.ID = fmt.Sprintf("device-%d", f.next)
	}
	f.devices[device.ID] = device
	return device
}

// Minted are the keys the tailnet was asked for, in order.
func (f *FakeControl) Minted() []KeyRequest {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	return slices.Clone(f.keys)
}

// Joined are the ids of the devices the tailnet has now.
func (f *FakeControl) Joined() []string {
	f.mutex.Lock()
	defer f.mutex.Unlock()
	ids := make([]string, 0, len(f.devices))
	for id := range f.devices {
		ids = append(ids, id)
	}
	slices.Sort(ids)
	return ids
}

// FakeDialer is a Dialer for tests: it records what was dialed and answers with
// what Handle returns, or with ErrNotFound when there is no Handle.
type FakeDialer struct {
	mutex sync.Mutex
	// dialed are the addresses, in order, as "network address".
	dialed []string
	// Handle answers a dial.
	Handle func(ctx context.Context, network, address string) (net.Conn, error)
}

// Dial records the address and answers with the handler's connection.
func (d *FakeDialer) Dial(ctx context.Context, network, address string) (net.Conn, error) {
	d.mutex.Lock()
	d.dialed = append(d.dialed, network+" "+address)
	handle := d.Handle
	d.mutex.Unlock()

	if handle == nil {
		return nil, fmt.Errorf("%w: %s %s", ErrNotFound, network, address)
	}
	return handle(ctx, network, address)
}

// Dialed are the addresses that were dialed, in order.
func (d *FakeDialer) Dialed() []string {
	d.mutex.Lock()
	defer d.mutex.Unlock()
	return slices.Clone(d.dialed)
}
