package plugins

import (
	"fmt"
	"strconv"
	"strings"
)

// Version is a semantic version: MAJOR.MINOR.PATCH, an optional pre-release and
// an optional build, as semver.org describes them.
type Version struct {
	Major, Minor, Patch uint64
	// Pre is the pre-release, such as "rc.1"; empty is a release.
	Pre string
	// Build is the build metadata, which takes no part in comparisons.
	Build string
}

// ParseVersion reads a version. A leading "v" is accepted, and parts after the
// major may be left out ("1.2" is 1.2.0), which is how a constraint names a
// range.
func ParseVersion(s string) (Version, error) {
	original := s
	s = strings.TrimPrefix(s, "v")
	if s == "" {
		return Version{}, fmt.Errorf("%w: empty version", ErrInvalid)
	}

	var version Version
	core, rest, _ := strings.Cut(s, "+")
	if rest != "" {
		version.Build = rest
		if !identifiers(rest) {
			return Version{}, fmt.Errorf("%w: version %q has a bad build", ErrInvalid, original)
		}
	}
	core, pre, found := strings.Cut(core, "-")
	if found {
		if !identifiers(pre) {
			return Version{}, fmt.Errorf("%w: version %q has a bad pre-release", ErrInvalid, original)
		}
		version.Pre = pre
	}

	parts := strings.Split(core, ".")
	if len(parts) > 3 {
		return Version{}, fmt.Errorf("%w: version %q has more than three parts", ErrInvalid, original)
	}
	numbers := make([]uint64, 3)
	for i, part := range parts {
		n, err := parseNumber(part)
		if err != nil {
			return Version{}, fmt.Errorf("%w: version %q: %w", ErrInvalid, original, err)
		}
		numbers[i] = n
	}
	version.Major, version.Minor, version.Patch = numbers[0], numbers[1], numbers[2]
	return version, nil
}

// MustParseVersion is ParseVersion for a version written in code.
func MustParseVersion(s string) Version {
	version, err := ParseVersion(s)
	if err != nil {
		panic(err)
	}
	return version
}

// parseNumber reads one numeric part, and refuses leading zeroes beyond "0",
// which semver does too.
func parseNumber(part string) (uint64, error) {
	if part == "" {
		return 0, fmt.Errorf("a part is empty")
	}
	for _, r := range part {
		if r < '0' || r > '9' {
			return 0, fmt.Errorf("%q is not a number", part)
		}
	}
	if len(part) > 1 && part[0] == '0' {
		return 0, fmt.Errorf("%q has a leading zero", part)
	}
	n, err := strconv.ParseUint(part, 10, 64)
	if err != nil {
		return 0, fmt.Errorf("%q is not a number: %w", part, err)
	}
	return n, nil
}

// identifiers reports whether a pre-release or a build is dot-separated
// alphanumerics and hyphens.
func identifiers(s string) bool {
	if s == "" {
		return false
	}
	for _, part := range strings.Split(s, ".") {
		if part == "" {
			return false
		}
		for _, r := range part {
			switch {
			case r >= 'a' && r <= 'z', r >= 'A' && r <= 'Z', r >= '0' && r <= '9', r == '-':
			default:
				return false
			}
		}
	}
	return true
}

// String is the version as semver writes it.
func (v Version) String() string {
	out := fmt.Sprintf("%d.%d.%d", v.Major, v.Minor, v.Patch)
	if v.Pre != "" {
		out += "-" + v.Pre
	}
	if v.Build != "" {
		out += "+" + v.Build
	}
	return out
}

// Compare orders two versions: -1 when v is older, 0 when they are the same
// release and 1 when it is newer. Build metadata takes no part, as semver says.
func (v Version) Compare(other Version) int {
	switch {
	case v.Major != other.Major:
		return compareNumbers(v.Major, other.Major)
	case v.Minor != other.Minor:
		return compareNumbers(v.Minor, other.Minor)
	case v.Patch != other.Patch:
		return compareNumbers(v.Patch, other.Patch)
	}
	return comparePre(v.Pre, other.Pre)
}

func compareNumbers(a, b uint64) int {
	switch {
	case a < b:
		return -1
	case a > b:
		return 1
	}
	return 0
}

// comparePre orders pre-releases: a release is newer than any pre-release of it,
// numeric identifiers are older than alphanumeric ones, and a shorter set of
// identifiers that is a prefix of a longer one is older.
func comparePre(a, b string) int {
	switch {
	case a == "" && b == "":
		return 0
	case a == "":
		return 1
	case b == "":
		return -1
	}
	left, right := strings.Split(a, "."), strings.Split(b, ".")
	for i := 0; i < len(left) && i < len(right); i++ {
		if left[i] == right[i] {
			continue
		}
		leftNumber, leftErr := strconv.ParseUint(left[i], 10, 64)
		rightNumber, rightErr := strconv.ParseUint(right[i], 10, 64)
		switch {
		case leftErr == nil && rightErr == nil:
			return compareNumbers(leftNumber, rightNumber)
		case leftErr == nil:
			return -1
		case rightErr == nil:
			return 1
		case left[i] < right[i]:
			return -1
		default:
			return 1
		}
	}
	return compareNumbers(uint64(len(left)), uint64(len(right)))
}

// Requirement is what a manifest says about the versions it works with, such as
// "^1.2" or ">=0.1.0, <2.0.0". The empty requirement allows everything.
type Requirement []Constraint

// ErrNoVersion is what Resolve answers when no version satisfies it.
var ErrNoVersion = fmt.Errorf("%w: no version satisfies it", ErrInvalid)

// ParseRequirement reads a requirement: constraints separated by commas or
// spaces. All of them must hold.
func ParseRequirement(s string) (Requirement, error) {
	requirement := Requirement{}
	for _, field := range strings.FieldsFunc(s, func(r rune) bool {
		return r == ',' || r == ' ' || r == '\t'
	}) {
		constraint, err := ParseConstraint(field)
		if err != nil {
			return nil, err
		}
		requirement = append(requirement, constraint)
	}
	return requirement, nil
}

// Allows reports whether a version satisfies every constraint.
func (r Requirement) Allows(v Version) bool {
	for _, constraint := range r {
		if !constraint.Allows(v) {
			return false
		}
	}
	return true
}

// String is the requirement as it would be written in a manifest.
func (r Requirement) String() string {
	parts := make([]string, 0, len(r))
	for _, constraint := range r {
		parts = append(parts, constraint.String())
	}
	return strings.Join(parts, ", ")
}

// Constraint is one bound of a requirement.
type Constraint struct {
	// Op is one of "", "=", ">", ">=", "<", "<=", "^" and "~".
	Op string
	// Version is the version the bound is about.
	Version Version
	// parts is how many numbers the constraint wrote, which "~" reads: "~1"
	// allows 1.x.y, "~1.2" allows 1.2.x.
	parts int
}

// ParseConstraint reads one constraint, such as ">=1.2.3" or "^2".
func ParseConstraint(s string) (Constraint, error) {
	op := ""
	for _, candidate := range []string{">=", "<=", "^", "~", ">", "<", "=", "=="} {
		if strings.HasPrefix(s, candidate) {
			op = candidate
			break
		}
	}
	written := strings.TrimPrefix(s, op)
	version, err := ParseVersion(written)
	if err != nil {
		return Constraint{}, fmt.Errorf("constraint %q: %w", s, err)
	}
	if op == "==" {
		op = "="
	}
	parts := len(strings.Split(strings.Split(written, "-")[0], "."))
	return Constraint{Op: op, Version: version, parts: parts}, nil
}

// String is the constraint as a manifest writes it.
func (c Constraint) String() string { return c.Op + c.Version.String() }

// Allows reports whether a version satisfies the constraint.
func (c Constraint) Allows(v Version) bool {
	switch c.Op {
	case ">":
		return v.Compare(c.Version) > 0
	case ">=":
		return v.Compare(c.Version) >= 0
	case "<":
		return v.Compare(c.Version) < 0
	case "<=":
		return v.Compare(c.Version) <= 0
	case "^":
		return v.Compare(c.Version) >= 0 && v.Compare(c.compatible()) < 0
	case "~":
		return v.Compare(c.Version) >= 0 && v.Compare(c.patchCompatible()) < 0
	default:
		// An exact version: pre-release and build aside, semver's "=".
		return v.Compare(c.Version) == 0
	}
}

// compatible is the version after the one a caret allows: ^1.2.3 allows
// 1.x.y from 1.2.3, and ^0.2.3 and ^0.0.3 allow no new minor or patch, since a
// 0.x release may break anything.
func (c Constraint) compatible() Version {
	switch {
	case c.Version.Major > 0:
		return Version{Major: c.Version.Major + 1}
	case c.Version.Minor > 0:
		return Version{Minor: c.Version.Minor + 1}
	default:
		return Version{Patch: c.Version.Patch + 1}
	}
}

// patchCompatible is the version after the one a tilde allows: ~1.2.3 and ~1.2
// allow 1.2.x, and ~1 allows 1.x.y.
func (c Constraint) patchCompatible() Version {
	if c.parts <= 1 && c.parts > 0 {
		return Version{Major: c.Version.Major + 1}
	}
	return Version{Major: c.Version.Major, Minor: c.Version.Minor + 1}
}
