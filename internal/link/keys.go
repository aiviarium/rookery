package link

import (
	"context"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
)

// KeysService is KeyService: the key ring, and the host keys the server trusts.
// Only an admin reaches it; the access option on its procedures says so.
type KeysService struct {
	ring  *Ring
	known *KnownHosts
}

// NewKeysService returns the key service.
func NewKeysService(ring *Ring, known *KnownHosts) *KeysService {
	return &KeysService{ring: ring, known: known}
}

// List returns the keys of the ring, newest first.
func (s *KeysService) List(ctx context.Context, _ *connect.Request[rookeryv1.KeyServiceListRequest]) (*connect.Response[rookeryv1.KeyServiceListResponse], error) {
	active, err := s.ring.Active(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	keys, err := s.ring.List(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	response := &rookeryv1.KeyServiceListResponse{}
	for _, key := range keys {
		response.Keys = append(response.Keys, keyMessage(key, key.ID == active.ID))
	}
	return connect.NewResponse(response), nil
}

// Rotate makes a new key active and retires the old one.
func (s *KeysService) Rotate(ctx context.Context, request *connect.Request[rookeryv1.KeyServiceRotateRequest]) (*connect.Response[rookeryv1.KeyServiceRotateResponse], error) {
	key, previous, err := s.ring.Rotate(ctx, request.Msg.GetName())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	response := &rookeryv1.KeyServiceRotateResponse{Key: keyMessage(*key, true)}
	if previous != nil {
		response.Previous = keyMessage(*previous, false)
	}
	return connect.NewResponse(response), nil
}

// ListKnownHosts returns the host keys the server trusts.
func (s *KeysService) ListKnownHosts(ctx context.Context, _ *connect.Request[rookeryv1.KeyServiceListKnownHostsRequest]) (*connect.Response[rookeryv1.KeyServiceListKnownHostsResponse], error) {
	hosts, err := s.known.List(ctx)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	response := &rookeryv1.KeyServiceListKnownHostsResponse{}
	for _, host := range hosts {
		known := &rookeryv1.KnownHost{
			Address:            host.Address,
			Host:               host.Host,
			Type:               host.KeyType,
			PublicKey:          host.PublicKey,
			Fingerprint:        host.Fingerprint,
			PendingFingerprint: host.PendingFingerprint,
		}
		response.Hosts = append(response.Hosts, known)
	}
	return connect.NewResponse(response), nil
}

// TrustHostKey trusts the key a host offered when it did not match.
func (s *KeysService) TrustHostKey(ctx context.Context, request *connect.Request[rookeryv1.KeyServiceTrustHostKeyRequest]) (*connect.Response[rookeryv1.KeyServiceTrustHostKeyResponse], error) {
	host, err := s.known.Trust(ctx, request.Msg.GetAddress(), request.Msg.GetFingerprint())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.KeyServiceTrustHostKeyResponse{
		Host: &rookeryv1.KnownHost{
			Address:     host.Address,
			Host:        host.Host,
			Type:        host.KeyType,
			PublicKey:   host.PublicKey,
			Fingerprint: host.Fingerprint,
		},
	}), nil
}

// ForgetHostKey deletes a trusted key.
func (s *KeysService) ForgetHostKey(ctx context.Context, request *connect.Request[rookeryv1.KeyServiceForgetHostKeyRequest]) (*connect.Response[rookeryv1.KeyServiceForgetHostKeyResponse], error) {
	if err := s.known.Forget(ctx, request.Msg.GetAddress()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.KeyServiceForgetHostKeyResponse{}), nil
}

// keyMessage is a key as the API has it.
func keyMessage(key Key, active bool) *rookeryv1.Key {
	message := &rookeryv1.Key{
		Id:          key.ID,
		Name:        key.Name,
		Type:        key.Record.KeyType,
		PublicKey:   key.Public,
		Fingerprint: key.Fingerprint,
		CreatedAt:   timestamppb.New(key.Record.CreatedAt),
		Active:      active,
	}
	if key.Record.RetiredAt != nil {
		message.RetiredAt = timestamppb.New(*key.Record.RetiredAt)
	}
	return message
}
