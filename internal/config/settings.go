// Package config is what a deployment is set up with, apart from its code:
// settings with scopes and defaults, secrets sealed under the deployment key,
// and connections to outside services.
//
// A setting resolves user, then space, then global, then its declared default.
// A secret is sealed with XChaCha20-Poly1305 under the deployment key, with the
// row's name and scope as associated data and the key recorded per row, so that
// a rekey reseals the rows one at a time. A connection's fields are sealed as
// one value the same way.
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
	"github.com/aiviarium/rookery/internal/store"
)

// Scopes of a setting and of a secret.
const (
	ScopeGlobal = "global"
	ScopeSpace  = "space"
	ScopeUser   = "user"
)

// Definition is what a setting is: its type, its default and where it may be
// set.
type Definition struct {
	// Key is such as rookery/instance_name; the core's start with rookery/.
	Key string
	// Type is string, int, float, bool, time or cbor.
	Type string
	// Default is the value nobody set, and is always encoded.
	Default []byte
	// Scopes are the scopes the setting has.
	Scopes []string
	// Title is its name as a person reads it, and Description what it changes.
	Title       string
	Description string
	// Category groups it within its plugin.
	Category string
	// Choices are the only values a string setting takes, when set.
	Choices []string
	// Minimum and Maximum bound a number, when set.
	Minimum *float64
	Maximum *float64
	// Unit follows a number, such as seconds.
	Unit string
}

// plugin is the plugin a key belongs to: the part before its first slash.
func (d Definition) plugin() string {
	if at := strings.IndexByte(d.Key, '/'); at > 0 {
		return d.Key[:at]
	}
	return "rookery"
}

// CoreSettings are the settings the core declares.
func CoreSettings() []Definition {
	return []Definition{
		{
			Key: "rookery/instance_name", Type: "string", Default: mustCBOR("Rookery"), Scopes: []string{ScopeGlobal},
			Title: "Name", Category: "General",
			Description: "The name the shell shows in its sidebar and the browser shows on its tab.",
		},
		{
			Key: "rookery/timezone", Type: "string", Default: mustCBOR("UTC"), Scopes: []string{ScopeGlobal, ScopeUser},
			Title: "Time zone", Category: "General",
			Description: "The IANA time zone schedules and times are shown in, such as Europe/Berlin.",
		},
		{
			Key: "rookery/notifications", Type: "bool", Default: mustCBOR(true), Scopes: []string{ScopeGlobal, ScopeUser},
			Title: "Notifications", Category: "General",
			Description: "Whether the shell tells you when a job you started ends or a host needs attention.",
		},
	}
}

// Settings reads and writes setting values.
type Settings struct {
	store       *store.Store
	mu          sync.RWMutex
	definitions map[string]Definition
}

// NewSettings returns the settings of a database, with the definitions the core
// declares. A plugin adds its own with Define.
func NewSettings(opened *store.Store, definitions ...Definition) *Settings {
	settings := &Settings{store: opened, definitions: make(map[string]Definition)}
	for _, definition := range append(CoreSettings(), definitions...) {
		settings.definitions[definition.Key] = definition
	}
	return settings
}

// Define adds a setting's definition.
func (s *Settings) Define(definition Definition) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if definition.Key == "" {
		return errors.New("config: a setting needs a key")
	}
	if len(definition.Scopes) == 0 {
		return fmt.Errorf("config: the setting %s declares no scope", definition.Key)
	}
	if _, seen := s.definitions[definition.Key]; seen {
		return fmt.Errorf("config: the setting %s is declared already", definition.Key)
	}
	if definition.Default == nil {
		definition.Default = mustCBOR(nil)
	}
	s.definitions[definition.Key] = definition
	return nil
}

// Definitions are the settings this deployment knows.
func (s *Settings) Definitions() []Definition {
	s.mu.RLock()
	defer s.mu.RUnlock()
	out := make([]Definition, 0, len(s.definitions))
	for _, definition := range s.definitions {
		out = append(out, definition)
	}
	return out
}

// Value is a setting as it resolved for a caller.
type Value struct {
	Key       string
	Type      string
	Default   []byte
	Scopes    []string
	Value     []byte
	Scope     string
	UpdatedAt time.Time
	// Definition is what the setting is.
	Definition Definition
	// Values are the values set at each scope that applies to the caller.
	Values map[string][]byte
	// Editable are the scopes the caller may set it at, once SetEditable has run.
	Editable []string
}

// Message is a setting as the edge API has it.
func (v Value) Message() *rookeryv1.Setting {
	out := &rookeryv1.Setting{
		Key:         v.Key,
		Type:        v.Type,
		DefaultCbor: v.Default,
		Scopes:      v.Scopes,
		ValueCbor:   v.Value,
		Scope:       v.Scope,
		Title:       v.Definition.Title,
		Description: v.Definition.Description,
		Category:    v.Definition.Category,
		Plugin:      v.Definition.plugin(),
		Choices:     v.Definition.Choices,
		Minimum:     v.Definition.Minimum,
		Maximum:     v.Definition.Maximum,
		Unit:        v.Definition.Unit,
		ValuesCbor:  v.Values,
		Editable:    v.Editable,
	}
	if !v.UpdatedAt.IsZero() {
		out.UpdatedAt = timestamppb.New(v.UpdatedAt)
	}
	return out
}

// Get resolves a setting for a caller and the space it is working in.
func (s *Settings) Get(ctx context.Context, key string, principal access.Principal, space string) (Value, error) {
	definition, known := s.definition(key)
	if !known {
		return Value{}, fmt.Errorf("%w: there is no setting %s", access.ErrNotFound, key)
	}

	value := Value{
		Key:     definition.Key,
		Type:    definition.Type,
		Default: definition.Default,
		Scopes:  definition.Scopes,
		Value:   definition.Default,
		Scope:   "default",

		Definition: definition,
		Values:     map[string][]byte{},
	}

	// The nearest scope that has a value wins: user, then space, then global.
	type lookup struct {
		scope string
		id    string
	}
	var lookups []lookup
	if principal.Kind == access.KindUser {
		lookups = append(lookups, lookup{scope: ScopeUser, id: principal.ID})
	}
	if space != "" {
		lookups = append(lookups, lookup{scope: ScopeSpace, id: space})
	}
	lookups = append(lookups, lookup{scope: ScopeGlobal})

	// Every scope's value is read, for a page that shows what each scope says;
	// the nearest one resolves.
	resolved := false
	for _, candidate := range lookups {
		if !hasScope(definition, candidate.scope) {
			continue
		}
		record, err := s.store.Queries().GetSetting(ctx, store.GetSettingParams{
			Key:     key,
			Scope:   candidate.scope,
			ScopeID: candidate.id,
		})
		switch {
		case errors.Is(err, pgx.ErrNoRows):
			continue
		case err != nil:
			return Value{}, fmt.Errorf("config: reading the setting %s: %w", key, err)
		}
		value.Values[candidate.scope] = record.Value
		if !resolved {
			value.Value = record.Value
			value.Scope = candidate.scope
			value.UpdatedAt = record.UpdatedAt
			resolved = true
		}
	}
	return value, nil
}

// List resolves every setting the deployment knows for a caller.
func (s *Settings) List(ctx context.Context, principal access.Principal, space string) ([]Value, error) {
	definitions := s.Definitions()
	keys := make([]string, 0, len(definitions))
	for _, definition := range definitions {
		keys = append(keys, definition.Key)
	}
	sortStrings(keys)

	out := make([]Value, 0, len(keys))
	for _, key := range keys {
		value, err := s.Get(ctx, key, principal, space)
		if errors.Is(err, access.ErrNotFound) {
			continue
		}
		if err != nil {
			return nil, err
		}
		out = append(out, value)
	}
	return out, nil
}

// Set writes a setting at a scope. A global value is an admin's, a space's is
// for whoever manages the space, and a user's is the caller's own.
func (s *Settings) Set(ctx context.Context, key, scope, space string, value []byte, principal access.Principal, checker *access.Checker) error {
	definition, known := s.definition(key)
	if !known {
		return fmt.Errorf("%w: there is no setting %s", access.ErrNotFound, key)
	}
	if !hasScope(definition, scope) {
		return fmt.Errorf("%w: the setting %s has no %s scope", access.ErrInvalid, key, scope)
	}

	scopeID, err := s.scopeID(ctx, scope, space, principal, checker)
	if err != nil {
		return err
	}
	if err := checkValue(definition, value); err != nil {
		return err
	}
	if err := checkBounds(definition, value); err != nil {
		return err
	}

	if err := s.store.Queries().SetSetting(ctx, store.SetSettingParams{
		Key:     key,
		Scope:   scope,
		ScopeID: scopeID,
		Value:   value,
	}); err != nil {
		return fmt.Errorf("config: setting %s: %w", key, err)
	}
	return nil
}

// Reset removes a setting's value at a scope, so the nearer or the default value
// resolves again.
func (s *Settings) Reset(ctx context.Context, key, scope, space string, principal access.Principal, checker *access.Checker) error {
	definition, known := s.definition(key)
	if !known {
		return fmt.Errorf("%w: there is no setting %s", access.ErrNotFound, key)
	}
	if !hasScope(definition, scope) {
		return fmt.Errorf("%w: the setting %s has no %s scope", access.ErrInvalid, key, scope)
	}
	scopeID, err := s.scopeID(ctx, scope, space, principal, checker)
	if err != nil {
		return err
	}
	if _, err := s.store.Queries().DeleteSetting(ctx, store.DeleteSettingParams{Key: key, Scope: scope, ScopeID: scopeID}); err != nil {
		return fmt.Errorf("config: resetting %s: %w", key, err)
	}
	return nil
}

// scopeID is the row a value goes on, after checking the caller may write there.
func (s *Settings) scopeID(ctx context.Context, scope, space string, principal access.Principal, checker *access.Checker) (string, error) {
	switch scope {
	case ScopeGlobal:
		if !principal.IsAdmin() {
			return "", fmt.Errorf("%w: a global setting is an admin's", access.ErrDenied)
		}
		return "", nil
	case ScopeSpace:
		if space == "" {
			return "", fmt.Errorf("%w: a space setting names its space", access.ErrInvalid)
		}
		if err := checker.Check(ctx, principal, access.SpaceURI(space), access.Manage); err != nil {
			return "", err
		}
		return space, nil
	case ScopeUser:
		if !principal.Authenticated() {
			return "", access.ErrUnauthenticated
		}
		return principal.ID, nil
	default:
		return "", fmt.Errorf("%w: %q is not a setting scope", access.ErrInvalid, scope)
	}
}

// hasScope reports whether a definition has a scope.
func hasScope(definition Definition, scope string) bool {
	for _, candidate := range definition.Scopes {
		if candidate == scope {
			return true
		}
	}
	return false
}

// checkValue refuses a value that is not the definition's type.
func checkValue(definition Definition, value []byte) error {
	if len(value) == 0 {
		return fmt.Errorf("%w: the setting %s has no value", access.ErrInvalid, definition.Key)
	}
	switch definition.Type {
	case "string":
		var text string
		if err := cborx.Unmarshal(value, &text); err != nil {
			return fmt.Errorf("%w: the setting %s is a string: %w", access.ErrInvalid, definition.Key, err)
		}
	case "int":
		var number int64
		if err := cborx.Unmarshal(value, &number); err != nil {
			return fmt.Errorf("%w: the setting %s is an integer: %w", access.ErrInvalid, definition.Key, err)
		}
	case "float":
		var number float64
		if err := cborx.Unmarshal(value, &number); err != nil {
			return fmt.Errorf("%w: the setting %s is a number: %w", access.ErrInvalid, definition.Key, err)
		}
	case "bool":
		var flag bool
		if err := cborx.Unmarshal(value, &flag); err != nil {
			return fmt.Errorf("%w: the setting %s is true or false: %w", access.ErrInvalid, definition.Key, err)
		}
	case "time", "cbor":
		// Any CBOR value.
	default:
		return fmt.Errorf("%w: the setting %s has the unknown type %q", access.ErrInvalid, definition.Key, definition.Type)
	}
	return nil
}

// checkBounds refuses a choice the definition does not offer and a number out
// of its bounds.
func checkBounds(definition Definition, value []byte) error {
	if len(definition.Choices) > 0 && definition.Type == "string" {
		var text string
		if err := cborx.Unmarshal(value, &text); err == nil {
			for _, choice := range definition.Choices {
				if choice == text {
					return nil
				}
			}
			return fmt.Errorf("%w: the setting %s is one of %s", access.ErrInvalid, definition.Key, strings.Join(definition.Choices, ", "))
		}
	}
	if (definition.Type == "int" || definition.Type == "float") && (definition.Minimum != nil || definition.Maximum != nil) {
		var number float64
		if err := cborx.Unmarshal(value, &number); err == nil {
			if definition.Minimum != nil && number < *definition.Minimum {
				return fmt.Errorf("%w: the setting %s is at least %g", access.ErrInvalid, definition.Key, *definition.Minimum)
			}
			if definition.Maximum != nil && number > *definition.Maximum {
				return fmt.Errorf("%w: the setting %s is at most %g", access.ErrInvalid, definition.Key, *definition.Maximum)
			}
		}
	}
	return nil
}

// EditableScopes are the scopes a caller may write in the space they work in:
// global for an admin, space for whoever manages it, user for a signed-in user.
func EditableScopes(ctx context.Context, principal access.Principal, space string, checker *access.Checker) []string {
	var scopes []string
	if principal.Kind == access.KindUser {
		scopes = append(scopes, ScopeUser)
	}
	if space != "" && checker != nil && checker.Check(ctx, principal, access.SpaceURI(space), access.Manage) == nil {
		scopes = append(scopes, ScopeSpace)
	}
	if principal.IsAdmin() {
		scopes = append(scopes, ScopeGlobal)
	}
	return scopes
}

// SetEditable marks the scopes of a value the caller may write, from the scopes
// it may write anywhere.
func (v *Value) SetEditable(scopes []string) {
	v.Editable = nil
	for _, scope := range scopes {
		if hasScope(v.Definition, scope) {
			v.Editable = append(v.Editable, scope)
		}
	}
}

// mustCBOR encodes a definition's default, which cannot fail.
func mustCBOR(value any) []byte {
	encoded, err := cborx.Marshal(value)
	if err != nil {
		panic(fmt.Sprintf("config: encoding a default: %v", err))
	}
	return encoded
}

// sortStrings sorts in place, for a listing that reads the same every time.
func sortStrings(values []string) {
	for i := 1; i < len(values); i++ {
		for j := i; j > 0 && values[j] < values[j-1]; j-- {
			values[j], values[j-1] = values[j-1], values[j]
		}
	}
}

// SettingsService is SettingService.
type SettingsService struct {
	settings *Settings
	checker  *access.Checker
}

// NewSettingsService returns SettingService for a database.
func NewSettingsService(settings *Settings, checker *access.Checker) *SettingsService {
	return &SettingsService{settings: settings, checker: checker}
}

// List resolves every setting the deployment knows for the caller.
func (s *SettingsService) List(ctx context.Context, _ *connect.Request[rookeryv1.SettingServiceListRequest]) (*connect.Response[rookeryv1.SettingServiceListResponse], error) {
	principal := access.MustPrincipal(ctx)
	space := access.SpaceFrom(ctx)
	values, err := s.settings.List(ctx, principal, space)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	editable := EditableScopes(ctx, principal, space, s.checker)
	out := make([]*rookeryv1.Setting, 0, len(values))
	for _, value := range values {
		value.SetEditable(editable)
		out = append(out, value.Message())
	}
	return connect.NewResponse(&rookeryv1.SettingServiceListResponse{Settings: out}), nil
}

// Get resolves one setting.
func (s *SettingsService) Get(ctx context.Context, request *connect.Request[rookeryv1.SettingServiceGetRequest]) (*connect.Response[rookeryv1.SettingServiceGetResponse], error) {
	principal := access.MustPrincipal(ctx)
	space := request.Msg.GetSpace()
	if space == "" {
		space = access.SpaceFrom(ctx)
	}
	value, err := s.settings.Get(ctx, request.Msg.GetKey(), principal, space)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	value.SetEditable(EditableScopes(ctx, principal, space, s.checker))
	return connect.NewResponse(&rookeryv1.SettingServiceGetResponse{Setting: value.Message()}), nil
}

// Set writes a setting at a scope.
func (s *SettingsService) Set(ctx context.Context, request *connect.Request[rookeryv1.SettingServiceSetRequest]) (*connect.Response[rookeryv1.SettingServiceSetResponse], error) {
	principal := access.MustPrincipal(ctx)
	space := request.Msg.GetSpace()
	if space == "" {
		space = access.SpaceFrom(ctx)
	}
	scope := valueOr(request.Msg.GetScope(), ScopeGlobal)
	if err := s.settings.Set(ctx, request.Msg.GetKey(), scope, space, request.Msg.GetValueCbor(), principal, s.checker); err != nil {
		return nil, access.ToConnect(err)
	}
	value, err := s.settings.Get(ctx, request.Msg.GetKey(), principal, space)
	if err != nil {
		return nil, access.ToConnect(err)
	}
	value.SetEditable(EditableScopes(ctx, principal, space, s.checker))
	return connect.NewResponse(&rookeryv1.SettingServiceSetResponse{Setting: value.Message()}), nil
}

// Reset removes a setting's value at a scope.
func (s *SettingsService) Reset(ctx context.Context, request *connect.Request[rookeryv1.SettingServiceResetRequest]) (*connect.Response[rookeryv1.SettingServiceResetResponse], error) {
	principal := access.MustPrincipal(ctx)
	space := request.Msg.GetSpace()
	if space == "" {
		space = access.SpaceFrom(ctx)
	}
	scope := valueOr(request.Msg.GetScope(), ScopeGlobal)
	if err := s.settings.Reset(ctx, request.Msg.GetKey(), scope, space, principal, s.checker); err != nil {
		return nil, access.ToConnect(err)
	}
	return connect.NewResponse(&rookeryv1.SettingServiceResetResponse{}), nil
}

// valueOr is a value, or a fallback when it is empty.
func valueOr(value, fallback string) string {
	if strings.TrimSpace(value) == "" {
		return fallback
	}
	return value
}

func (s *Settings) definition(key string) (Definition, bool) {
	s.mu.RLock()
	defer s.mu.RUnlock()
	definition, ok := s.definitions[key]
	return definition, ok
}

// ReplacePluginDefinitions refreshes the declarations of one plugin only.
func (s *Settings) ReplacePluginDefinitions(plugin string, definitions []Definition) {
	s.mu.Lock()
	defer s.mu.Unlock()
	prefix := plugin + "/"
	for key := range s.definitions {
		if strings.HasPrefix(key, prefix) {
			delete(s.definitions, key)
		}
	}
	for _, definition := range definitions {
		definition.Key = prefix + definition.Key
		s.definitions[definition.Key] = definition
	}
}
