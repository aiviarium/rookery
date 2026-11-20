package config

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"time"

	"connectrpc.com/connect"
	"github.com/jackc/pgx/v5"
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/access"
	"github.com/aiviarium/rookery/internal/cborx"
	"github.com/aiviarium/rookery/internal/id"
	"github.com/aiviarium/rookery/internal/seal"
	"github.com/aiviarium/rookery/internal/store"
)

// ConnectionType is a kind of connection, declared by the core or a plugin.
type ConnectionType struct {
	ID     string
	Label  string
	Fields []ConnectionField
	// Test is set when the declarer can test a connection of this type.
	Test   bool
	Plugin string
}

// ConnectionField is one field of a connection type.
type ConnectionField struct {
	Name     string
	Label    string
	Secret   bool
	Required bool
}

// CoreConnectionTypes are the types the core declares.
func CoreConnectionTypes() []ConnectionType {
	return []ConnectionType{{
		ID:    "registry",
		Label: "Container registry",
		Fields: []ConnectionField{
			{Name: "url", Label: "URL", Required: true},
			{Name: "username", Label: "Username", Required: true},
			{Name: "password", Label: "Password", Secret: true, Required: true},
		},
	}}
}

// Connections are a deployment's external credentials.
type Connections struct {
	store      *store.Store
	ring       *seal.Ring
	types      map[string]ConnectionType
	mu         sync.RWMutex
	testPlugin func(context.Context, ConnectionType, map[string]string) error
}

// NewConnections returns the connections of a database.
func NewConnections(opened *store.Store, ring *seal.Ring, types ...ConnectionType) *Connections {
	connections := &Connections{store: opened, ring: ring, types: make(map[string]ConnectionType)}
	for _, declared := range append(CoreConnectionTypes(), types...) {
		connections.types[declared.ID] = declared
	}
	return connections
}

// Types are the connection types this deployment declares.
func (c *Connections) Types() []ConnectionType {
	c.mu.RLock()
	defer c.mu.RUnlock()
	out := make([]ConnectionType, 0, len(c.types))
	for _, declared := range c.types {
		out = append(out, declared)
	}
	return out
}

// Type returns one type.
func (c *Connections) Type(id string) (ConnectionType, bool) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	declared, ok := c.types[id]
	return declared, ok
}

// Connection is a deployment's credentials of a type.
type Connection struct {
	ID        string
	Type      string
	Name      string
	Values    []byte
	CreatedAt time.Time
	UpdatedAt time.Time
}

// Message is a connection as the edge API has it: its secret fields hold
// nothing.
func (c Connection) Message() *rookeryv1.Connection {
	out := &rookeryv1.Connection{
		Id:         c.ID,
		Type:       c.Type,
		Name:       c.Name,
		ValuesCbor: c.Values,
	}
	if !c.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(c.CreatedAt)
	}
	if !c.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamppb.New(c.UpdatedAt)
	}
	return out
}

// Create makes a connection. values are its fields and are sealed as one value.
func (c *Connections) Create(ctx context.Context, connectionType, name string, values map[string]string) (Connection, error) {
	declared, known := c.Type(connectionType)
	if !known {
		return Connection{}, fmt.Errorf("%w: there is no connection type %q", access.ErrInvalid, connectionType)
	}
	name = strings.TrimSpace(name)
	if name == "" {
		return Connection{}, fmt.Errorf("%w: a connection needs a name", access.ErrInvalid)
	}
	encoded, err := encodeValues(declared, values)
	if err != nil {
		return Connection{}, err
	}

	connectionID := id.New()
	sealed, keyID, err := c.ring.Seal(encoded, []byte(connectionID))
	if err != nil {
		return Connection{}, err
	}
	record, err := c.store.Queries().CreateConnection(ctx, store.CreateConnectionParams{
		ID:     connectionID,
		Type:   connectionType,
		Name:   name,
		Values: sealed,
		KeyID:  keyID,
	})
	if err != nil {
		if isUniqueViolation(err) {
			return Connection{}, fmt.Errorf("%w: a connection called %s is there already", access.ErrConflict, name)
		}
		return Connection{}, fmt.Errorf("config: creating the connection %s: %w", name, err)
	}
	return connectionOf(record, declared), nil
}

// Update changes a connection's name or its fields: a field given keeps its
// value when the update leaves it out.
func (c *Connections) Update(ctx context.Context, connectionID string, name *string, values map[string]string) (Connection, error) {
	record, err := c.store.Queries().GetConnection(ctx, connectionID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Connection{}, fmt.Errorf("%w: no connection %s", access.ErrNotFound, connectionID)
	case err != nil:
		return Connection{}, fmt.Errorf("config: reading the connection %s: %w", connectionID, err)
	}
	declared, known := c.Type(record.Type)
	if !known {
		return Connection{}, fmt.Errorf("%w: the connection type %q is not declared any more", access.ErrUnavailable, record.Type)
	}

	plain, err := c.ring.Open(record.Values, record.KeyID, []byte(record.ID))
	if err != nil {
		return Connection{}, fmt.Errorf("config: opening the connection %s: %w", connectionID, err)
	}
	current := make(map[string]string)
	if err := cborx.Unmarshal(plain, &current); err != nil {
		return Connection{}, fmt.Errorf("config: reading the connection %s: %w", connectionID, err)
	}
	for field, value := range values {
		current[field] = value
	}
	encoded, err := encodeValues(declared, current)
	if err != nil {
		return Connection{}, err
	}

	newName := record.Name
	if name != nil && strings.TrimSpace(*name) != "" {
		newName = strings.TrimSpace(*name)
	}
	sealed, keyID, err := c.ring.Seal(encoded, []byte(record.ID))
	if err != nil {
		return Connection{}, err
	}
	updated, err := c.store.Queries().UpdateConnection(ctx, store.UpdateConnectionParams{
		ID:     record.ID,
		Name:   newName,
		Values: sealed,
		KeyID:  keyID,
	})
	if err != nil {
		if isUniqueViolation(err) {
			return Connection{}, fmt.Errorf("%w: a connection called %s is there already", access.ErrConflict, newName)
		}
		return Connection{}, fmt.Errorf("config: updating the connection %s: %w", connectionID, err)
	}
	return connectionOf(updated, declared), nil
}

// ByID returns a connection with its secret fields held out.
func (c *Connections) ByID(ctx context.Context, connectionID string) (Connection, error) {
	record, err := c.store.Queries().GetConnection(ctx, connectionID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return Connection{}, fmt.Errorf("%w: no connection %s", access.ErrNotFound, connectionID)
	case err != nil:
		return Connection{}, fmt.Errorf("config: reading the connection %s: %w", connectionID, err)
	}
	declared, _ := c.Type(record.Type)
	return connectionOf(record, declared), nil
}

// List returns a type's connections, or every type's.
func (c *Connections) List(ctx context.Context, connectionType string) ([]Connection, error) {
	records, err := c.store.Queries().ListConnections(ctx, optional(strings.TrimSpace(connectionType)))
	if err != nil {
		return nil, fmt.Errorf("config: listing connections: %w", err)
	}
	out := make([]Connection, 0, len(records))
	for _, record := range records {
		declared, _ := c.Type(record.Type)
		out = append(out, connectionOf(record, declared))
	}
	return out, nil
}

// Delete removes a connection.
func (c *Connections) Delete(ctx context.Context, connectionID string) error {
	deleted, err := c.store.Queries().DeleteConnection(ctx, connectionID)
	switch {
	case err != nil:
		return fmt.Errorf("config: deleting the connection %s: %w", connectionID, err)
	case deleted == 0:
		return fmt.Errorf("%w: no connection %s", access.ErrNotFound, connectionID)
	}
	return nil
}

// Values opens a connection's fields: what the apply writes on a host.
func (c *Connections) Values(ctx context.Context, connectionID string) (map[string]string, error) {
	record, err := c.store.Queries().GetConnection(ctx, connectionID)
	switch {
	case errors.Is(err, pgx.ErrNoRows):
		return nil, fmt.Errorf("%w: no connection %s", access.ErrNotFound, connectionID)
	case err != nil:
		return nil, fmt.Errorf("config: reading the connection %s: %w", connectionID, err)
	}
	return c.openValues(record)
}

// NamedValues opens the values of a connection of the expected type.
func (c *Connections) NamedValues(ctx context.Context, name, expectedType string) (map[string]string, error) {
	record, err := c.store.Queries().GetConnectionByName(ctx, name)
	if errors.Is(err, pgx.ErrNoRows) {
		return nil, fmt.Errorf("%w: no connection %s", access.ErrNotFound, name)
	}
	if err != nil {
		return nil, fmt.Errorf("config: reading connection %s: %w", name, err)
	}
	if record.Type != expectedType {
		return nil, fmt.Errorf("%w: connection %s must have type %s", access.ErrInvalid, name, expectedType)
	}
	return c.openValues(record)
}

func (c *Connections) openValues(record store.RookeryConnections) (map[string]string, error) {
	plain, err := c.ring.Open(record.Values, record.KeyID, []byte(record.ID))
	if err != nil {
		return nil, fmt.Errorf("config: opening the connection %s: %w", record.ID, err)
	}
	values := make(map[string]string)
	if err := cborx.Unmarshal(plain, &values); err != nil {
		return nil, fmt.Errorf("config: reading the connection %s: %w", record.ID, err)
	}
	return values, nil
}

// Test says whether a connection answers. The core's `registry` is checked when
// it is applied, not here; a plugin's test arrives with the plugin system.
func (c *Connections) Test(ctx context.Context, connectionID string) (bool, string, error) {
	connection, err := c.ByID(ctx, connectionID)
	if err != nil {
		return false, "", err
	}
	declared, known := c.Type(connection.Type)
	if !known || !declared.Test {
		return false, "the " + connection.Type + " type is checked when it is used", nil
	}
	if c.testPlugin == nil {
		return false, "", access.ErrUnavailable
	}
	values, err := c.Values(ctx, connectionID)
	if err != nil {
		return false, "", err
	}
	if err := c.testPlugin(ctx, declared, values); err != nil {
		return false, "The plugin rejected this connection.", nil
	}
	return true, "Connection test passed.", nil
}

// connectionOf is a connection from the database, with secret fields left out of
// the values the caller sees.
func connectionOf(record store.RookeryConnections, declared ConnectionType) Connection {
	connection := Connection{
		ID:        record.ID,
		Type:      record.Type,
		Name:      record.Name,
		CreatedAt: record.CreatedAt,
	}
	connection.UpdatedAt = record.UpdatedAt
	// The caller gets the non-secret fields; the sealed value stays sealed.
	if values, err := publicValues(record.Values, declared); err == nil {
		connection.Values = values
	}
	return connection
}

// publicValues is a connection's fields without the ones it declares as secret.
// It does not open the sealed value: a secret field is in there and nothing else
// names it, so what is left is what the row's name and type say. The values are
// read through Values, which only the server calls.
func publicValues(_ []byte, declared ConnectionType) ([]byte, error) {
	fields := make(map[string]string, len(declared.Fields))
	for _, field := range declared.Fields {
		if field.Secret {
			continue
		}
		fields[field.Name] = ""
	}
	return cborx.Marshal(fields)
}

// encodeValues checks a connection's fields against its type and encodes them.
func encodeValues(declared ConnectionType, values map[string]string) ([]byte, error) {
	known := make(map[string]ConnectionField, len(declared.Fields))
	for _, field := range declared.Fields {
		known[field.Name] = field
	}

	encoded := make(map[string]string, len(values))
	for name, value := range values {
		if _, ok := known[name]; !ok {
			return nil, fmt.Errorf("%w: the %s type has no field %q", access.ErrInvalid, declared.ID, name)
		}
		encoded[name] = value
	}
	for _, field := range declared.Fields {
		if field.Required && encoded[field.Name] == "" {
			return nil, fmt.Errorf("%w: a %s connection needs %s", access.ErrInvalid, declared.ID, field.Name)
		}
	}
	raw, err := cborx.Marshal(encoded)
	if err != nil {
		return nil, err
	}
	return raw, nil
}

// ConnectionsService is ConnectionService.
type ConnectionsService struct {
	connections *Connections
}

// NewConnectionsService returns ConnectionService for a database.
func NewConnectionsService(connections *Connections) *ConnectionsService {
	return &ConnectionsService{connections: connections}
}

// ListTypes returns the declared types.
func (s *ConnectionsService) ListTypes(context.Context, *connect.Request[rookeryv1.ConnectionServiceListTypesRequest]) (*connect.Response[rookeryv1.ConnectionServiceListTypesResponse], error) {
	out := make([]*rookeryv1.ConnectionType, 0)
	for _, declared := range s.connections.Types() {
		converted := &rookeryv1.ConnectionType{
			Id:     declared.ID,
			Label:  declared.Label,
			Test:   declared.Test,
			Plugin: declared.Plugin,
		}
		for _, field := range declared.Fields {
			converted.Fields = append(converted.Fields, &rookeryv1.ConnectionField{
				Name:     field.Name,
				Label:    field.Label,
				Secret:   field.Secret,
				Required: field.Required,
			})
		}
		out = append(out, converted)
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceListTypesResponse{Types: out}), nil
}

// List returns the connections of a type, or of every type.
func (s *ConnectionsService) List(ctx context.Context, request *connect.Request[rookeryv1.ConnectionServiceListRequest]) (*connect.Response[rookeryv1.ConnectionServiceListResponse], error) {
	found, err := s.connections.List(ctx, request.Msg.GetType())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	out := make([]*rookeryv1.Connection, 0, len(found))
	for _, connection := range found {
		out = append(out, connection.Message())
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceListResponse{Connections: out}), nil
}

// Get returns one connection.
func (s *ConnectionsService) Get(ctx context.Context, request *connect.Request[rookeryv1.ConnectionServiceGetRequest]) (*connect.Response[rookeryv1.ConnectionServiceGetResponse], error) {
	connection, err := s.connections.ByID(ctx, request.Msg.GetConnection())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceGetResponse{Connection: connection.Message()}), nil
}

// Create makes a connection.
func (s *ConnectionsService) Create(ctx context.Context, request *connect.Request[rookeryv1.ConnectionServiceCreateRequest]) (*connect.Response[rookeryv1.ConnectionServiceCreateResponse], error) {
	values, err := decodeValues(request.Msg.GetValuesCbor())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	connection, err := s.connections.Create(ctx, request.Msg.GetType(), request.Msg.GetName(), values)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceCreateResponse{Connection: connection.Message()}), nil
}

// Update changes a connection.
func (s *ConnectionsService) Update(ctx context.Context, request *connect.Request[rookeryv1.ConnectionServiceUpdateRequest]) (*connect.Response[rookeryv1.ConnectionServiceUpdateResponse], error) {
	var values map[string]string
	if request.Msg.ValuesCbor != nil {
		decoded, err := decodeValues(request.Msg.GetValuesCbor())
		if err != nil {
			return nil, access.ToConnect(err)
		}
		values = decoded
	}
	connection, err := s.connections.Update(ctx, request.Msg.GetConnection(), request.Msg.Name, values)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceUpdateResponse{Connection: connection.Message()}), nil
}

// Delete removes a connection.
func (s *ConnectionsService) Delete(ctx context.Context, request *connect.Request[rookeryv1.ConnectionServiceDeleteRequest]) (*connect.Response[rookeryv1.ConnectionServiceDeleteResponse], error) {
	if err := s.connections.Delete(ctx, request.Msg.GetConnection()); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceDeleteResponse{}), nil
}

// Test says whether a connection answers.
func (s *ConnectionsService) Test(ctx context.Context, request *connect.Request[rookeryv1.ConnectionServiceTestRequest]) (*connect.Response[rookeryv1.ConnectionServiceTestResponse], error) {
	ok, message, err := s.connections.Test(ctx, request.Msg.GetConnection())
	if err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.ConnectionServiceTestResponse{Ok: ok, Message: message}), nil
}

// decodeValues reads a connection's fields from CBOR.
func decodeValues(raw []byte) (map[string]string, error) {
	if len(raw) == 0 {
		return map[string]string{}, nil
	}
	values := make(map[string]string)
	if err := cborx.Unmarshal(raw, &values); err != nil {
		return nil, fmt.Errorf("%w: a connection's values are a CBOR map of strings: %w", access.ErrInvalid, err)
	}
	return values, nil
}

// isUniqueViolation reports a Postgres unique constraint error.
func isUniqueViolation(err error) bool {
	var pgErr interface{ SQLState() string }
	return errors.As(err, &pgErr) && pgErr.SQLState() == "23505"
}

// SetPluginTester installs the callback used for declared connection tests.
func (c *Connections) SetPluginTester(test func(context.Context, ConnectionType, map[string]string) error) {
	c.testPlugin = test
}

// ReplacePluginTypes refreshes the declarations of one plugin.
func (c *Connections) ReplacePluginTypes(plugin string, types []ConnectionType) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for id, declared := range c.types {
		if declared.Plugin == plugin {
			delete(c.types, id)
		}
	}
	for _, declared := range types {
		declared.ID = plugin + "/" + declared.ID
		declared.Plugin = plugin
		c.types[declared.ID] = declared
	}
}
