package authn

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"errors"
	"fmt"
	"strings"

	"github.com/coreos/go-oidc/v3/oidc"
	"golang.org/x/oauth2"

	"github.com/aiviarium/rookery/internal/id"
)

// OIDCConfig is what a deployment's identity provider is set up with, from
// ROOKERY_OIDC_ISSUER, ROOKERY_OIDC_CLIENT_ID, ROOKERY_OIDC_CLIENT_SECRET,
// ROOKERY_OIDC_GROUPS_CLAIM and ROOKERY_ADMINS.
type OIDCConfig struct {
	// Issuer is the provider's issuer URL. Empty turns OIDC off.
	Issuer string
	// ClientID and ClientSecret are the deployment's OAuth client.
	ClientID     string
	ClientSecret string
	// RedirectURL is where the provider sends the browser back: the edge's
	// /auth/oidc/callback.
	RedirectURL string
	// GroupsClaim is the claim whose values bind a user to groups, by default
	// "groups".
	GroupsClaim string
	// Admins are emails that start as admins on first sign-in.
	Admins []string
}

// OIDC signs users in at an identity provider.
type OIDC struct {
	provider    *oidc.Provider
	verifier    *oidc.IDTokenVerifier
	oauth       oauth2.Config
	groupsClaim string
	admins      map[string]bool
}

// Discover talks to the provider and returns it. The provider is reached once
// at start-up, so a wrong issuer fails there rather than at the first sign-in.
func Discover(ctx context.Context, config OIDCConfig) (*OIDC, error) {
	provider, err := oidc.NewProvider(ctx, config.Issuer)
	if err != nil {
		return nil, fmt.Errorf("authn: discovering the OIDC provider %s: %w", config.Issuer, err)
	}
	groupsClaim := config.GroupsClaim
	if groupsClaim == "" {
		groupsClaim = "groups"
	}
	admins := make(map[string]bool, len(config.Admins))
	for _, email := range config.Admins {
		admins[normalizeEmail(email)] = true
	}
	return &OIDC{
		provider: provider,
		verifier: provider.Verifier(&oidc.Config{ClientID: config.ClientID}),
		oauth: oauth2.Config{
			ClientID:     config.ClientID,
			ClientSecret: config.ClientSecret,
			Endpoint:     provider.Endpoint(),
			RedirectURL:  config.RedirectURL,
			Scopes:       []string{oidc.ScopeOpenID, "profile", "email", groupsClaim},
		},
		groupsClaim: groupsClaim,
		admins:      admins,
	}, nil
}

// StartURL is where the browser goes to sign in, with the state and PKCE
// verifier it will come back with.
func (o *OIDC) StartURL(state, verifier string) string {
	return o.oauth.AuthCodeURL(state,
		oauth2.SetAuthURLParam("code_challenge", Challenge(verifier)),
		oauth2.SetAuthURLParam("code_challenge_method", "S256"),
	)
}

// Identity is who a provider says the caller is.
type Identity struct {
	// Subject is the provider's stable id for the person.
	Subject string
	Email   string
	Name    string
	// Groups is the groups claim's values.
	Groups []string
}

// Exchange turns the provider's code into what the person claims about
// themselves, checking the ID token's signature and audience.
func (o *OIDC) Exchange(ctx context.Context, code, verifier string) (Identity, error) {
	token, err := o.oauth.Exchange(ctx, code, oauth2.SetAuthURLParam("code_verifier", verifier))
	if err != nil {
		return Identity{}, fmt.Errorf("authn: exchanging the OIDC code: %w", err)
	}
	rawID, ok := token.Extra("id_token").(string)
	if !ok || rawID == "" {
		return Identity{}, fmt.Errorf("%w: the provider returned no id_token", ErrOIDC)
	}
	idToken, err := o.verifier.Verify(ctx, rawID)
	if err != nil {
		return Identity{}, fmt.Errorf("%w: the id_token does not verify: %w", ErrOIDC, err)
	}

	var claims struct {
		Email  string   `json:"email"`
		Name   string   `json:"name"`
		Login  string   `json:"preferred_username"`
		Groups []string `json:"groups"`
	}
	// The groups claim's name is the deployment's, so it is read twice: by the
	// struct's fixed field and as a map for any other name.
	if err := idToken.Claims(&claims); err != nil {
		return Identity{}, fmt.Errorf("%w: reading the id_token's claims: %w", ErrOIDC, err)
	}
	groups := claims.Groups
	if o.groupsClaim != "groups" {
		var other map[string]any
		if err := idToken.Claims(&other); err != nil {
			return Identity{}, fmt.Errorf("%w: reading the id_token's claims: %w", ErrOIDC, err)
		}
		groups = claimValues(other[o.groupsClaim])
	}

	email := normalizeEmail(claims.Email)
	if email != "" {
		if err := checkEmail(email); err != nil {
			return Identity{}, err
		}
	}
	name := strings.TrimSpace(claims.Name)
	if name == "" {
		name = claims.Login
	}
	if name == "" && email != "" {
		name, _, _ = strings.Cut(email, "@")
	}

	return Identity{Subject: idToken.Subject, Email: email, Name: name, Groups: groups}, nil
}

// Admins reports whether an email is one ROOKERY_ADMINS lists.
func (o *OIDC) Admins(email string) bool { return o.admins[normalizeEmail(email)] }

// GroupsClaim is the claim whose values bind groups.
func (o *OIDC) GroupsClaim() string { return o.groupsClaim }

// NewState is a random string the browser carries through the provider and back.
func NewState() string { return id.Secret(24) }

// NewVerifier is a PKCE verifier; Challenge is the S256 challenge made from it.
func NewVerifier() string { return id.Secret(32) }

// Challenge is base64url(SHA-256(verifier)), without padding.
func Challenge(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64.RawURLEncoding.EncodeToString(sum[:])
}

// VerifyChallenge reports whether a verifier belongs to a challenge.
func VerifyChallenge(verifier, wanted string) bool {
	return Challenge(verifier) == wanted
}

// CallbackURL is the provider's redirect for a public address.
func CallbackURL(publicURL string) string {
	return strings.TrimRight(publicURL, "/") + "/auth/oidc/callback"
}

// ErrOIDC is a failed OIDC sign-in, which the edge shows as a page rather than
// as an RPC error.
var ErrOIDC = errors.New("authn: OIDC sign-in failed")

// claimValues reads a claim as a list of strings: the providers send either an
// array or one string.
func claimValues(value any) []string {
	switch typed := value.(type) {
	case []any:
		out := make([]string, 0, len(typed))
		for _, item := range typed {
			if text, ok := item.(string); ok && text != "" {
				out = append(out, text)
			}
		}
		return out
	case []string:
		return typed
	case string:
		if typed == "" {
			return nil
		}
		return []string{typed}
	default:
		return nil
	}
}
