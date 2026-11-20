package authn

import (
	"google.golang.org/protobuf/types/known/timestamppb"

	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/internal/store"
)

// SessionMessage is a session as the edge API has it. current is the session the
// call came from, so a client can mark it.
func SessionMessage(session store.RookerySessions, current string) *rookeryv1.Session {
	out := &rookeryv1.Session{
		Id:        session.ID,
		UserId:    session.UserID,
		UserAgent: session.UserAgent,
		Current:   session.ID == current,
	}
	if !session.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(session.CreatedAt)
	}
	if !session.SeenAt.IsZero() {
		out.SeenAt = timestamppb.New(session.SeenAt)
	}
	return out
}

// TokenMessage is a token's metadata as the edge API has it.
func TokenMessage(token store.RookeryTokens) *rookeryv1.Token {
	out := &rookeryv1.Token{Id: token.ID, Name: token.Name}
	if !token.CreatedAt.IsZero() {
		out.CreatedAt = timestamppb.New(token.CreatedAt)
	}
	if token.LastUsedAt != nil && !token.LastUsedAt.IsZero() {
		out.LastUsedAt = timestamppb.New(*token.LastUsedAt)
	}
	return out
}

// LinkMessage is a link as the edge API has it, with the URL to hand over.
func LinkMessage(link store.RookeryLinks, url string) *rookeryv1.Link {
	out := &rookeryv1.Link{Url: url}
	if !link.ExpiresAt.IsZero() {
		out.ExpiresAt = timestamppb.New(link.ExpiresAt)
	}
	return out
}
