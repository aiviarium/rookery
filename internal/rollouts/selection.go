package rollouts

import (
	"context"
	"fmt"
	"strings"

	"github.com/aiviarium/rookery/internal/access"
)

// HostSelector is the host registry as a roll-out sees it: every host there is,
// with the labels and the applied state a selection reads, and why one of them
// cannot take this roll-out's work. The hosts item fills it in; until then a
// roll-out that names its hosts works without it.
type HostSelector interface {
	Hosts(ctx context.Context) ([]Candidate, error)
}

// Candidate is a host, as a selection reads it.
type Candidate struct {
	// Name is the host's name.
	Name string
	// Labels are the host's labels, such as role=desk.
	Labels map[string]string
	// Current is whether what the host runs is what its profile asks for.
	Current bool
	// Skip is why the host cannot take this roll-out's work, such as a
	// capability its type has not got. Empty when it can.
	Skip string
	// ActionSkip is an additional refusal for an action.
	ActionSkip string
}

// Term is one part of a label selector.
type Term struct {
	// Key is the label's name.
	Key string
	// Value is what it must be, or what it must not be when Different is set.
	Value string
	// Different is set for "key!=value".
	Different bool
	// Present, with an empty Key's value, asks that the label is there at all.
	Present bool
}

// Selector is a parsed label selector: a host is in when every term holds.
type Selector []Term

// ParseSelector reads a label selector, such as "role=desk,site!=lab".
func ParseSelector(expression string) (Selector, error) {
	expression = strings.TrimSpace(expression)
	if expression == "" {
		return nil, nil
	}

	var selector Selector
	for _, part := range strings.Split(expression, ",") {
		part = strings.TrimSpace(part)
		if part == "" {
			return nil, fmt.Errorf("%w: the label selector %q has an empty term", access.ErrInvalid, expression)
		}

		term := Term{}
		switch {
		case strings.Contains(part, "!="):
			key, value, _ := strings.Cut(part, "!=")
			term.Key, term.Value, term.Different = strings.TrimSpace(key), strings.TrimSpace(value), true
		case strings.Contains(part, "="):
			key, value, _ := strings.Cut(part, "=")
			term.Key, term.Value = strings.TrimSpace(key), strings.TrimSpace(value)
		default:
			term.Key, term.Present = part, true
		}
		if term.Key == "" || strings.ContainsAny(term.Key, " \t") {
			return nil, fmt.Errorf("%w: %q is not a label in %q", access.ErrInvalid, part, expression)
		}
		selector = append(selector, term)
	}
	return selector, nil
}

// Match reports whether a host with these labels is in the selector.
func (s Selector) Match(labels map[string]string) bool {
	for _, term := range s {
		value, has := labels[term.Key]
		switch {
		case term.Present:
			if !has {
				return false
			}
		case term.Different:
			if has && value == term.Value {
				return false
			}
		default:
			if !has || value != term.Value {
				return false
			}
		}
	}
	return true
}

// String is the selector as it was written.
func (s Selector) String() string {
	parts := make([]string, 0, len(s))
	for _, term := range s {
		switch {
		case term.Present:
			parts = append(parts, term.Key)
		case term.Different:
			parts = append(parts, term.Key+"!="+term.Value)
		default:
			parts = append(parts, term.Key+"="+term.Value)
		}
	}
	return strings.Join(parts, ",")
}

// matches reports whether a host takes part in a roll-out with this selection.
// The criteria combine: a host the selection names by name, that has the labels,
// and that is behind when the selection asks for that.
func matches(candidate Candidate, selection Selection, selector Selector) bool {
	if len(selection.Hosts) > 0 && !contains(selection.Hosts, candidate.Name) {
		return false
	}
	if selection.Labels != "" && !selector.Match(candidate.Labels) {
		return false
	}
	if selection.Behind && candidate.Current {
		return false
	}
	return true
}

// contains reports whether a list holds a value.
func contains(values []string, value string) bool {
	for _, candidate := range values {
		if candidate == value {
			return true
		}
	}
	return false
}
