package provisioner

// The labels the provisioner sets on what it makes, and the compose labels it
// keeps. A resource is matched to its record by user.rookery.entry and
// user.rookery.name, and found again by user.rookery.managed.
const (
	// LabelPrefix is the prefix of the labels the provisioner sets.
	LabelPrefix = "user.rookery."

	// LabelManaged marks everything the provisioner made: "true".
	LabelManaged = LabelPrefix + "managed"

	// LabelEntry is the entry that made it.
	LabelEntry = LabelPrefix + "entry"

	// LabelKind is the resource kind it is.
	LabelKind = LabelPrefix + "kind"

	// LabelName is what the entry called it, and for a machine the host's name.
	LabelName = LabelPrefix + "name"

	// ComposeProject and ComposeService are what compose puts on a project's
	// containers, which the provisioner keeps so that a deployment is what
	// `docker compose` would have made.
	ComposeProject = "com.docker.compose.project"
	ComposeService = "com.docker.compose.service"
)
