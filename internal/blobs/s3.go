package blobs

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/url"
	"os"
	"path/filepath"
	"strings"

	"github.com/minio/minio-go/v7"
	"github.com/minio/minio-go/v7/pkg/credentials"
)

// S3Config is how a blob bucket is reached.
type S3Config struct {
	// Endpoint is the host, such as s3.amazonaws.com or minio.example.test:9000.
	Endpoint string
	// AccessKey and SecretKey are the credentials; empty takes the standard
	// environment's.
	AccessKey string
	SecretKey string
	// Region is the bucket's region, empty for the endpoint's default.
	Region string
	// Secure turns on TLS.
	Secure bool
	// UsePathStyle is set for an S3 that wants the bucket in the path.
	UsePathStyle bool
}

// S3 is a backend that keeps blobs in a bucket, under a prefix.
type S3 struct {
	client *minio.Client
	bucket string
	prefix string
}

// NewS3 returns an S3 backend for s3://bucket/prefix.
func NewS3(location string, config S3Config) (*S3, error) {
	bucket, prefix, err := parseS3Location(location)
	if err != nil {
		return nil, err
	}

	accessKey, secretKey := config.AccessKey, config.SecretKey
	if accessKey == "" {
		accessKey = os.Getenv("ROOKERY_S3_ACCESS_KEY")
	}
	if secretKey == "" {
		secretKey = os.Getenv("ROOKERY_S3_SECRET_KEY")
	}
	endpoint := config.Endpoint
	if endpoint == "" {
		endpoint = os.Getenv("ROOKERY_S3_ENDPOINT")
	}
	if endpoint == "" {
		return nil, errors.New("blobs: an s3 location needs ROOKERY_S3_ENDPOINT")
	}

	client, err := minio.New(endpoint, &minio.Options{
		Creds:        credentials.NewStaticV4(accessKey, secretKey, ""),
		Secure:       config.Secure,
		Region:       config.Region,
		BucketLookup: bucketLookup(config.UsePathStyle),
	})
	if err != nil {
		return nil, fmt.Errorf("blobs: connecting to %s: %w", endpoint, err)
	}
	return &S3{client: client, bucket: bucket, prefix: prefix}, nil
}

// Put stores a stream and returns its SHA-256 and its size. The bytes go to a
// temporary file first, because a bucket wants the size before the body.
func (s *S3) Put(ctx context.Context, body io.Reader) (string, int64, error) {
	directory, err := os.MkdirTemp("", "rookery-blobs-")
	if err != nil {
		return "", 0, fmt.Errorf("blobs: making a temporary directory: %w", err)
	}
	defer func() { _ = os.RemoveAll(directory) }()

	temporaryPath := filepath.Join(directory, "blob")
	temporary, err := os.Create(temporaryPath) //nolint:gosec // the directory is ours
	if err != nil {
		return "", 0, fmt.Errorf("blobs: making a temporary file: %w", err)
	}

	id, size, err := checksumAndCopy(ctx, temporary, body)
	if err != nil {
		_ = temporary.Close()
		return "", 0, err
	}
	if err := temporary.Close(); err != nil {
		return "", 0, fmt.Errorf("blobs: writing the temporary file: %w", err)
	}

	if _, err := s.client.FPutObject(ctx, s.bucket, s.key(id), temporaryPath, minio.PutObjectOptions{
		ContentType: "application/octet-stream",
	}); err != nil {
		return "", 0, fmt.Errorf("blobs: uploading %s: %w", id, err)
	}
	return id, size, nil
}

// Open returns a blob's bytes.
func (s *S3) Open(ctx context.Context, id string) (io.ReadSeekCloser, int64, error) {
	object, err := s.client.GetObject(ctx, s.bucket, s.key(id), minio.GetObjectOptions{})
	if err != nil {
		return nil, 0, fmt.Errorf("blobs: reading %s: %w", id, err)
	}
	info, err := object.Stat()
	if err != nil {
		_ = object.Close()
		return nil, 0, fmt.Errorf("blobs: reading %s: %w", id, err)
	}
	return object, info.Size, nil
}

// Delete removes a blob's bytes.
func (s *S3) Delete(ctx context.Context, id string) error {
	if err := s.client.RemoveObject(ctx, s.bucket, s.key(id), minio.RemoveObjectOptions{}); err != nil {
		return fmt.Errorf("blobs: deleting %s: %w", id, err)
	}
	return nil
}

// key is where a blob is in the bucket.
func (s *S3) key(id string) string {
	if s.prefix == "" {
		return id
	}
	return strings.Trim(s.prefix, "/") + "/" + id
}

// parseS3Location splits s3://bucket/prefix.
func parseS3Location(location string) (bucket, prefix string, err error) {
	parsed, err := url.Parse(location)
	if err != nil {
		return "", "", fmt.Errorf("blobs: %q is not an s3 location: %w", location, err)
	}
	if parsed.Scheme != "s3" || parsed.Host == "" {
		return "", "", fmt.Errorf("blobs: %q is not an s3 location, such as s3://bucket/prefix", location)
	}
	return parsed.Host, strings.Trim(parsed.Path, "/"), nil
}

// bucketLookup is how a bucket is addressed: in the path, or in the host name.
func bucketLookup(pathStyle bool) minio.BucketLookupType {
	if pathStyle {
		return minio.BucketLookupPath
	}
	return minio.BucketLookupAuto
}
