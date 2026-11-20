package profile

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"

	"github.com/aiviarium/rookery/internal/cborx"
)

// DigestInput is what the applied digest is taken over: the profile, the
// versions of the plugins it names and Rookery's version. It is a structure of
// its own so that the encoding cannot drift with a field added to Profile
// without the digest changing on purpose.
type DigestInput struct {
	Profile Profile           `cbor:"profile"`
	Plugins map[string]string `cbor:"plugins"`
	Rookery string            `cbor:"rookery"`
}

// Digest is the applied digest: the SHA-256, in hex, of the Core Deterministic
// CBOR of the profile, the versions of the plugins its features name and
// Rookery's version. A host whose stored digest is this one is up to date; any
// difference, in the profile, a plugin's version or the server's, makes it
// behind.
//
// The profile is normalized first, so a profile that leaves out a default and one
// that states it have the same digest.
func Digest(p Profile, pluginVersions map[string]string, rookeryVersion string) (string, error) {
	normalized := p.Normalize()
	if err := normalized.Validate(); err != nil {
		return "", err
	}

	// Only the plugins the profile names count, so installing a plugin no host
	// uses leaves every digest alone.
	versions := make(map[string]string, len(normalized.Features))
	for _, feature := range normalized.Features {
		versions[feature.Plugin] = pluginVersions[feature.Plugin]
	}

	encoded, err := cborx.Marshal(DigestInput{
		Profile: normalized,
		Plugins: versions,
		Rookery: rookeryVersion,
	})
	if err != nil {
		return "", fmt.Errorf("profile: encoding the digest: %w", err)
	}
	sum := sha256.Sum256(encoded)
	return hex.EncodeToString(sum[:]), nil
}
