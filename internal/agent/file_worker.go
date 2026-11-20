package agent

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"syscall"

	"connectrpc.com/connect"
	"google.golang.org/protobuf/proto"

	hostv1 "github.com/aiviarium/rookery/gen/go/rookery/host/v1"
)

type fileReply struct {
	Data  []byte
	Code  connect.Code
	Error string
}

func runFileWorker[Response any, Message interface {
	*Response
	proto.Message
}](ctx context.Context, operation string, acct *account, credential *syscall.Credential, request proto.Message, response Message) (*connect.Response[Response], error) {
	executable, err := os.Executable()
	if err != nil {
		return nil, toConnect(fmt.Errorf("agent: finding the file worker: %w", err))
	}
	data, err := proto.Marshal(request)
	if err != nil {
		return nil, toConnect(err)
	}
	cmd := exec.CommandContext(ctx, executable, "file-worker", operation) //nolint:gosec // re-exec the daemon with a fixed operation
	cmd.Stdin = bytes.NewReader(data)
	cmd.Env = acct.environ(nil)
	cmd.SysProcAttr = &syscall.SysProcAttr{Credential: credential}
	output, err := cmd.Output()
	if err != nil {
		if ctx.Err() != nil {
			return nil, toConnect(ctx.Err())
		}
		return nil, toConnect(fmt.Errorf("agent: running file worker: %w", err))
	}
	var reply fileReply
	if err := json.Unmarshal(output, &reply); err != nil {
		return nil, toConnect(fmt.Errorf("agent: decoding file worker response: %w", err))
	}
	if reply.Error != "" {
		return nil, connect.NewError(reply.Code, errors.New(reply.Error))
	}
	if err := proto.Unmarshal(reply.Data, response); err != nil {
		return nil, toConnect(fmt.Errorf("agent: decoding file response: %w", err))
	}
	return connect.NewResponse(response), nil
}

// FileWorker performs one file operation with the operating system permissions
// of the account the daemon set at exec. It only reads its command on stdin.
func FileWorker(ctx context.Context, operation string, input io.Reader, output io.Writer) error {
	acct, err := lookupAccountByID(uint32(os.Geteuid())) //nolint:gosec // a uid fits in uint32
	if err != nil {
		return err
	}
	ctx = context.WithValue(ctx, callerKey{}, &caller{role: roleRoot, account: acct})
	request, err := io.ReadAll(input)
	if err != nil {
		return fmt.Errorf("agent: reading file worker request: %w", err)
	}

	service := &fileService{}
	var data []byte
	switch operation {
	case "read":
		data, err = workerCall(ctx, request, &hostv1.FileServiceReadRequest{}, service.Read)
	case "write":
		data, err = workerCall(ctx, request, &hostv1.FileServiceWriteRequest{}, service.Write)
	case "list":
		data, err = workerCall(ctx, request, &hostv1.FileServiceListRequest{}, service.List)
	case "stat":
		data, err = workerCall(ctx, request, &hostv1.FileServiceStatRequest{}, service.Stat)
	case "delete":
		data, err = workerCall(ctx, request, &hostv1.FileServiceDeleteRequest{}, service.Delete)
	default:
		return fmt.Errorf("agent: unknown file operation %q", operation)
	}

	reply := fileReply{Data: data}
	if err != nil {
		reply.Code = connect.CodeOf(err)
		reply.Error = err.Error()
		var rpcErr *connect.Error
		if errors.As(err, &rpcErr) {
			reply.Error = rpcErr.Message()
		}
	}
	return json.NewEncoder(output).Encode(reply)
}

func workerCall[Request, Response any, In interface {
	*Request
	proto.Message
}, Out interface {
	*Response
	proto.Message
}](ctx context.Context, data []byte, message In, call func(context.Context, *connect.Request[Request]) (*connect.Response[Response], error)) ([]byte, error) {
	if err := proto.Unmarshal(data, message); err != nil {
		return nil, err
	}
	response, err := call(ctx, connect.NewRequest(message))
	if err != nil {
		return nil, err
	}
	return proto.Marshal(Out(response.Msg))
}
