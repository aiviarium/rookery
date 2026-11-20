package cli

import (
	"context"
	"fmt"
	"io"
	"os"
	"path"
	"path/filepath"
	"strings"

	"connectrpc.com/connect"
	"github.com/spf13/cobra"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
	rookeryv1 "github.com/aiviarium/rookery/gen/go/rookery/v1"
	"github.com/aiviarium/rookery/gen/go/rookery/v1/rookeryv1connect"
)

type remoteFile struct {
	host string
	path string
}

func splitRemote(value string) (remoteFile, bool) {
	if filepath.VolumeName(value) != "" {
		return remoteFile{}, false
	}
	host, remote, found := strings.Cut(value, ":")
	if !found || host == "" || strings.ContainsAny(host, "/\\") {
		return remoteFile{}, false
	}
	return remoteFile{host: host, path: remote}, true
}

func cpCommand() *cobra.Command {
	var account string
	command := &cobra.Command{
		Use:   "cp SRC DST",
		Short: "Copy a file to or from a host using NAME:PATH",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			source, fromHost := splitRemote(args[0])
			dest, toHost := splitRemote(args[1])
			if fromHost == toHost {
				return fmt.Errorf("cli: exactly one side of the copy must be NAME:PATH")
			}
			session, err := open(cmd)
			if err != nil {
				return err
			}
			ctx := cmd.Context()
			user := &hostv1.User{Name: account}
			if toHost {
				if dest.path == "" || strings.HasSuffix(dest.path, "/") {
					if args[0] == "-" {
						return fmt.Errorf("cli: give a remote filename when copying from stdin")
					}
					dest.path = path.Join(dest.path, filepath.Base(args[0]))
				}
				return uploadFile(ctx, cmd.InOrStdin(), session.api.Files, user, args[0], dest)
			}
			if source.path == "" {
				return fmt.Errorf("cli: a remote source needs a filename")
			}
			return downloadFile(ctx, cmd.OutOrStdout(), session.api.Files, user, source, args[1])
		},
	}
	command.Flags().StringVar(&account, "as", "", "desktop, operator or root")
	return command
}

func uploadFile(ctx context.Context, stdin io.Reader, files rookeryv1connect.FileServiceClient, user *hostv1.User, source string, dest remoteFile) error {
	mode := uint32(0)
	if source != "-" {
		file, err := os.Open(source) //nolint:gosec // the user names the local source
		if err != nil {
			return err
		}
		defer func() { _ = file.Close() }()
		info, err := file.Stat()
		if err != nil {
			return err
		}
		if !info.Mode().IsRegular() {
			return fmt.Errorf("cli: %s is not a regular file", source)
		}
		mode = uint32(info.Mode().Perm())
		stdin = file
	}
	const maxUpload = 64 << 20
	data, err := io.ReadAll(io.LimitReader(stdin, maxUpload+1))
	if err != nil {
		return fmt.Errorf("cli: reading source file: %w", err)
	}
	if len(data) > maxUpload {
		return fmt.Errorf("cli: a single file upload is limited to 64 MiB")
	}
	_, err = files.Write(ctx, connect.NewRequest(&rookeryv1.FileServiceWriteRequest{
		Host: dest.host,
		File: &hostv1.FileServiceWriteRequest{User: user, Path: dest.path, Data: data, Mode: mode},
	}))
	return err
}

func downloadFile(ctx context.Context, stdout io.Writer, files rookeryv1connect.FileServiceClient, user *hostv1.User, source remoteFile, dest string) error {
	if dest == "-" {
		return readRemoteFile(ctx, stdout, files, user, source)
	}
	if info, err := os.Stat(dest); err == nil && info.IsDir() {
		dest = filepath.Join(dest, path.Base(source.path))
	}
	file, err := os.CreateTemp(filepath.Dir(dest), ".rookery-cp-*")
	if err != nil {
		return err
	}
	defer func() { _ = os.Remove(file.Name()) }()
	copyErr := readRemoteFile(ctx, file, files, user, source)
	closeErr := file.Close()
	if copyErr != nil {
		return copyErr
	}
	if closeErr != nil {
		return closeErr
	}
	return os.Rename(file.Name(), dest)
}

func readRemoteFile(ctx context.Context, output io.Writer, files rookeryv1connect.FileServiceClient, user *hostv1.User, source remoteFile) error {
	var total uint64
	for offset := uint64(0); ; {
		response, err := files.Read(ctx, connect.NewRequest(&rookeryv1.FileServiceReadRequest{
			Host: source.host,
			File: &hostv1.FileServiceReadRequest{User: user, Path: source.path, Offset: offset, Length: 4 << 20},
		}))
		if err != nil {
			return err
		}
		result := response.Msg.GetResult()
		if offset == 0 {
			total = result.GetTotal()
		} else if total != result.GetTotal() {
			return fmt.Errorf("cli: remote file size changed during copy")
		}
		n, err := output.Write(result.GetData())
		if err != nil {
			return err
		}
		offset += uint64(n) //nolint:gosec // a successful write count is nonnegative
		if offset == total {
			return nil
		}
		if n == 0 || offset > total {
			return fmt.Errorf("cli: incomplete remote file")
		}
	}
}
