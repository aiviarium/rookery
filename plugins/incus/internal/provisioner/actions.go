package provisioner

import (
	"context"
	"fmt"
	"net/http"

	incus "github.com/lxc/incus/v6/client"
	"github.com/lxc/incus/v6/shared/api"

	"github.com/aiviarium/rookery/pkg/sdk"
)

// imagesAction lists the image aliases the server has, one line each.
func imagesAction(_ context.Context, server incus.InstanceServer, line func(string)) error {
	aliases, err := server.GetImageAliases()
	if err != nil {
		return fmt.Errorf("listing the server's images: %w", err)
	}
	for _, alias := range aliases {
		line(fmt.Sprintf("%s\t%s\t%s", alias.Name, alias.Type, alias.Description))
	}
	return nil
}

// instancesAction lists every instance on the server, whoever made it.
func instancesAction(_ context.Context, server incus.InstanceServer, line func(string)) error {
	found, err := server.GetInstances(api.InstanceTypeAny)
	if err != nil {
		return fmt.Errorf("listing the server's instances: %w", err)
	}
	for _, instance := range found {
		line(fmt.Sprintf("%s\t%s\t%s\t%s", instance.Name, instance.Type, instance.Status, instance.Description))
	}
	return nil
}

// deleteAction removes one instance by name, and says nothing when it is not
// there.
func deleteAction(_ context.Context, server incus.InstanceServer, args []byte, line func(string)) error {
	var request struct {
		Name string `cbor:"name"`
	}
	if len(args) > 0 {
		if err := sdk.Unmarshal(args, &request); err != nil {
			return fmt.Errorf("the arguments are not {name}: %w", err)
		}
	}
	if request.Name == "" {
		return fmt.Errorf("delete needs the instance's name")
	}
	operation, err := server.DeleteInstance(request.Name)
	if err != nil {
		if api.StatusErrorCheck(err, http.StatusNotFound) {
			line("there is no instance " + request.Name)
			return nil
		}
		return fmt.Errorf("removing %s: %w", request.Name, err)
	}
	if err := operation.Wait(); err != nil {
		return fmt.Errorf("removing %s: %w", request.Name, err)
	}
	line("removed " + request.Name)
	return nil
}
