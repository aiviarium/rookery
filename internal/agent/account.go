package agent

import (
	"bufio"
	"errors"
	"fmt"
	"os"
	"os/user"
	"strconv"
	"strings"
	"syscall"
)

// defaultPath is the PATH a command started for an account gets. It is the
// system's, not the daemon's, which systemd gives as little as possible.
const defaultPath = "/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin:/sbin"

// account is an account on the host: who a command runs as, and where its home,
// shell and environment are.
type account struct {
	Name   string
	UID    uint32
	GID    uint32
	Home   string
	Shell  string
	groups []uint32
}

// lookupAccount finds an account by name.
func lookupAccount(name string) (*account, error) {
	found, err := user.Lookup(name)
	if err != nil {
		return nil, fmt.Errorf("%w: the account %q: %w", ErrInvalid, name, err)
	}
	return fromUser(found, name)
}

// lookupAccountByID finds an account by uid. A uid without a passwd entry is
// still a caller: it gets a name to display and nothing else.
func lookupAccountByID(uid uint32) (*account, error) {
	found, err := user.LookupId(strconv.FormatUint(uint64(uid), 10))
	if err != nil {
		var unknown user.UnknownUserIdError
		if errors.As(err, &unknown) {
			return &account{
				Name:  fmt.Sprintf("uid-%d", uid),
				UID:   uid,
				Home:  "/",
				Shell: "/bin/sh",
			}, nil
		}
		return nil, fmt.Errorf("agent: looking up uid %d: %w", uid, err)
	}
	return fromUser(found, "")
}

func fromUser(found *user.User, name string) (*account, error) {
	uid, err := strconv.ParseUint(found.Uid, 10, 32)
	if err != nil {
		return nil, fmt.Errorf("agent: the uid %q of %s: %w", found.Uid, found.Username, err)
	}
	gid, err := strconv.ParseUint(found.Gid, 10, 32)
	if err != nil {
		return nil, fmt.Errorf("agent: the gid %q of %s: %w", found.Gid, found.Username, err)
	}
	if name == "" {
		name = found.Username
	}

	acct := &account{
		Name:  name,
		UID:   uint32(uid), //nolint:gosec // parsed as 32 bits.
		GID:   uint32(gid), //nolint:gosec // parsed as 32 bits.
		Home:  found.HomeDir,
		Shell: shellOf(name),
	}
	if groups, err := found.GroupIds(); err == nil {
		for _, group := range groups {
			if parsed, err := strconv.ParseUint(group, 10, 32); err == nil {
				acct.groups = append(acct.groups, uint32(parsed)) //nolint:gosec // parsed as 32 bits.
			}
		}
	}
	return acct, nil
}

// shellOf reads an account's login shell from /etc/passwd, which os/user does
// not report.
func shellOf(name string) string {
	file, err := os.Open("/etc/passwd")
	if err != nil {
		return "/bin/sh"
	}
	defer func() { _ = file.Close() }()

	scanner := bufio.NewScanner(file)
	for scanner.Scan() {
		fields := strings.Split(scanner.Text(), ":")
		if len(fields) >= 7 && fields[0] == name && fields[6] != "" {
			return fields[6]
		}
	}
	return "/bin/sh"
}

// credential is the syscall credential a command runs with, or nil when the
// daemon already runs as that account.
//
// Only root may act as another account: an unprivileged daemon, as a test runs,
// refuses, and says which account it is.
func (a *account) credential() (*syscall.Credential, error) {
	if a.UID == uint32(os.Geteuid()) { //nolint:gosec // a uid is a uint32.
		return nil, nil
	}
	if os.Geteuid() != 0 {
		return nil, fmt.Errorf("%w: %s runs as uid %d and may not act as %s",
			ErrNotPermitted, "rookeryd", os.Geteuid(), a.Name)
	}
	return &syscall.Credential{
		Uid:    a.UID,
		Gid:    a.GID,
		Groups: a.groups,
	}, nil
}

// environ is the environment a command runs with: the account's, with extra on
// top. It never carries the daemon's own environment, which holds nothing the
// account should see.
func (a *account) environ(extra map[string]string) []string {
	env := []string{
		"HOME=" + a.Home,
		"USER=" + a.Name,
		"LOGNAME=" + a.Name,
		"SHELL=" + a.loginShell(),
		"PATH=" + defaultPath,
	}
	if lang := os.Getenv("LANG"); lang != "" {
		env = append(env, "LANG="+lang)
	}
	for name, value := range extra {
		env = append(env, name+"="+value)
	}
	return env
}

// loginShell is the shell a session starts when the caller names no command.
func (a *account) loginShell() string {
	if a.Shell != "" {
		return a.Shell
	}
	return "/bin/sh"
}

// homeDir is the directory a command starts in when the caller names none.
func (a *account) homeDir() string {
	if a.Home != "" {
		return a.Home
	}
	return "/"
}
