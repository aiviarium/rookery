package agent

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
	"sync"
	"time"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	"github.com/aiviarium/rookery/internal/hostaction"
)

// The file the apply writes is /etc/rookery/actions.json, and its format is:
//
//	{
//	  "actions": [
//	    {
//	      "name": "wave",
//	      "label": "Wave",
//	      "group": "example",
//	      "user": "desktop",
//	      "description": "Waves at whoever is watching",
//	      "terminal": false,
//	      "command": "example-wave",
//	      "arguments": [
//	        {"name": "who", "label": "Who", "required": false, "default": "world"}
//	      ]
//	    }
//	  ]
//	}
//
// It holds the actions of the host's features that are marked onHost, which are
// the ones users on the host may run with `box run`; the apply leaves the rest
// out, because those belong to the server. `user` is one of "operator", "root"
// and "desktop" and says which account the action belongs to. `command` is run by
// `/bin/sh -c`, and the arguments never enter that string: they arrive as
// ROOKERY_ARG_<NAME> variables, upper-cased, so that a value cannot become part
// of the command.
//
// The file is read again when it changes, so applying a host takes effect without
// restarting the daemon.

// actionSet is the actions in the apply's file.
type actionSet struct {
	path string

	mu      sync.Mutex
	modTime time.Time
	size    int64
	actions []*action
}

// action is one action of a feature.
type action struct {
	name        string
	label       string
	group       string
	user        hostv1.ActionUser
	description string
	terminal    bool
	command     string
	arguments   []hostaction.Argument
}

// declaredAction and declaredArgument are the file's shape.
type declaredAction struct {
	Name        string                `json:"name"`
	Label       string                `json:"label"`
	Group       string                `json:"group"`
	User        string                `json:"user"`
	Description string                `json:"description"`
	Terminal    bool                  `json:"terminal"`
	Command     string                `json:"command"`
	Arguments   []hostaction.Argument `json:"arguments"`
}

// actionFile is the file's whole shape.
type actionFile struct {
	Actions []declaredAction `json:"actions"`
}

func newActionSet(path string) *actionSet {
	return &actionSet{path: path}
}

// list returns the actions, sorted by group and name, reading the file again when
// it changed.
func (s *actionSet) list() ([]*action, error) {
	if err := s.reload(); err != nil {
		return nil, err
	}

	s.mu.Lock()
	defer s.mu.Unlock()
	actions := make([]*action, len(s.actions))
	copy(actions, s.actions)
	sort.Slice(actions, func(i, j int) bool {
		if actions[i].group != actions[j].group {
			return actions[i].group < actions[j].group
		}
		return actions[i].name < actions[j].name
	})
	return actions, nil
}

// get returns one action by name.
func (s *actionSet) get(name string) (*action, error) {
	actions, err := s.list()
	if err != nil {
		return nil, err
	}
	for _, found := range actions {
		if found.name == name {
			return found, nil
		}
	}
	return nil, fmt.Errorf("%w: no action %s", os.ErrNotExist, name)
}

// reload parses the file when it changed since the last read. A host with no
// features has no file, which is no actions rather than an error.
func (s *actionSet) reload() error {
	info, err := os.Stat(s.path)
	if err != nil {
		if os.IsNotExist(err) {
			s.mu.Lock()
			s.actions, s.modTime, s.size = nil, time.Time{}, 0
			s.mu.Unlock()
			return nil
		}
		return fmt.Errorf("agent: reading %s: %w", s.path, err)
	}

	s.mu.Lock()
	unchanged := info.ModTime().Equal(s.modTime) && info.Size() == s.size
	s.mu.Unlock()
	if unchanged {
		return nil
	}

	raw, err := os.ReadFile(s.path) //nolint:gosec // the daemon's own configuration file.
	if err != nil {
		return fmt.Errorf("agent: reading %s: %w", s.path, err)
	}
	var parsed actionFile
	if err := json.Unmarshal(raw, &parsed); err != nil {
		return fmt.Errorf("agent: parsing %s: %w", s.path, err)
	}

	actions := make([]*action, 0, len(parsed.Actions))
	for _, declared := range parsed.Actions {
		parsedAction, err := parseAction(declared)
		if err != nil {
			return fmt.Errorf("agent: %s: %w", s.path, err)
		}
		actions = append(actions, parsedAction)
	}

	s.mu.Lock()
	s.actions, s.modTime, s.size = actions, info.ModTime(), info.Size()
	s.mu.Unlock()
	return nil
}

// parseAction checks one declared action.
func parseAction(declared declaredAction) (*action, error) {
	name := declared.Name
	if name == "" {
		return nil, fmt.Errorf("%w: an action has no name", ErrInvalid)
	}
	if declared.Command == "" {
		return nil, fmt.Errorf("%w: %s has no command", ErrInvalid, name)
	}

	actor, err := parseActionUser(declared.User)
	if err != nil {
		return nil, fmt.Errorf("%w: %s: %w", ErrInvalid, name, err)
	}
	group := declared.Group
	if group == "" {
		group = "host"
	}
	label := declared.Label
	if label == "" {
		label = name
	}

	parsed := &action{
		name:        name,
		label:       label,
		group:       group,
		user:        actor,
		description: declared.Description,
		terminal:    declared.Terminal,
		command:     declared.Command,
	}
	for _, argument := range declared.Arguments {
		if _, err := hostaction.Variable(argument.Name); err != nil {
			return nil, fmt.Errorf("%w: %s: %w", ErrInvalid, name, err)
		}
	}
	parsed.arguments = declared.Arguments
	return parsed, nil
}

// parseActionUser resolves the account kind an action belongs to.
func parseActionUser(user string) (hostv1.ActionUser, error) {
	switch strings.ToLower(user) {
	case "operator":
		return hostv1.ActionUser_ACTION_USER_OPERATOR, nil
	case "root":
		return hostv1.ActionUser_ACTION_USER_ROOT, nil
	case "desktop":
		return hostv1.ActionUser_ACTION_USER_DESKTOP, nil
	case "":
		return hostv1.ActionUser_ACTION_USER_UNSPECIFIED, fmt.Errorf("an action needs a user: operator, root or desktop")
	default:
		return hostv1.ActionUser_ACTION_USER_UNSPECIFIED, fmt.Errorf("unknown user %q: want operator, root or desktop", user)
	}
}

// proto is an action as the RPC reports it.
func (a *action) proto() *hostv1.Action {
	declared := &hostv1.Action{
		Name:        a.name,
		Label:       a.label,
		Group:       a.group,
		User:        a.user,
		OnHost:      true,
		Terminal:    a.terminal,
		Command:     a.command,
		Description: a.description,
		Arguments:   make([]*hostv1.ActionArgument, 0, len(a.arguments)),
	}
	for _, argument := range a.arguments {
		declared.Arguments = append(declared.Arguments, &hostv1.ActionArgument{
			Name:         argument.Name,
			Label:        argument.Label,
			Required:     argument.Required,
			DefaultValue: argument.Default,
		})
	}
	return declared
}
