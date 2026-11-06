// Command sh is poly's shell formatter component: shfmt (mvdan.cc/sh) behind
// alef's framed stdio transport. It runs as one long-lived child of poly,
// reads framed JSON requests on stdin, answers on stdout and logs to stderr.
package main

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
)

const (
	// statusOK and statusFailed are alef's AlefStatus codes.
	statusOK     = 0
	statusFailed = 3

	contractPoly = "poly"
	methodDescr  = "describe"
	methodCall   = "call"
)

func main() {
	manifest, err := loadManifest()
	if err == nil {
		err = run(os.Stdin, os.Stdout, manifest)
	}
	if err != nil && !errors.Is(err, io.EOF) {
		fmt.Fprintf(os.Stderr, "poly-sh: %v\n", err)
		os.Exit(1)
	}
}

// run serves frames until the host closes stdin.
func run(in io.Reader, out io.Writer, manifest componentManifest) error {
	srv := newServer(manifest.Identity.Version)
	reader := bufio.NewReader(in)
	for {
		frame, err := readFrame(reader)
		if err != nil {
			return err
		}
		var req envelope
		if err := json.Unmarshal(frame, &req); err != nil {
			return fmt.Errorf("decode request: %w", err)
		}
		answer, err := handle(srv, manifest, req)
		if err != nil {
			return err
		}
		body, err := json.Marshal(answer)
		if err != nil {
			return fmt.Errorf("encode reply: %w", err)
		}
		if err := writeFrame(out, body); err != nil {
			return err
		}
	}
}

// handle answers one envelope. Only an undecodable envelope is fatal; an
// unknown method or a failed call is a non-zero status.
func handle(srv *server, manifest componentManifest, req envelope) (reply, error) {
	switch req.Op {
	case "describe":
		body, err := json.Marshal(manifest.descriptor())
		if err != nil {
			return reply{}, fmt.Errorf("encode descriptor: %w", err)
		}
		return reply{Status: statusOK, Payload: body}, nil
	case "call":
		return handleCall(srv, req), nil
	default:
		return reply{}, fmt.Errorf("unknown op %q", req.Op)
	}
}

func handleCall(srv *server, req envelope) reply {
	if req.Contract != contractPoly {
		return failed("contract %q is not provided", req.Contract)
	}
	switch req.Method {
	case methodDescr:
		out, err := srv.describe()
		if err != nil {
			return failed("%v", err)
		}
		return reply{Status: statusOK, Payload: out}
	case methodCall:
		header, body, err := unpackCall(req.Payload)
		if err != nil {
			return failed("%v", err)
		}
		out, err := srv.call(header, body)
		if err != nil {
			return failed("%v", err)
		}
		return reply{Status: statusOK, Payload: out}
	default:
		return failed("method %q is not served", req.Method)
	}
}

func failed(format string, args ...any) reply {
	return reply{Status: statusFailed, Payload: []byte(fmt.Sprintf(format, args...))}
}

// unpackCall splits a `call` payload: a little-endian u32 header length, the
// MessagePack request header, then the file content (the rest).
func unpackCall(payload []byte) (header, body []byte, err error) {
	if len(payload) < 4 {
		return nil, nil, errors.New("call payload is shorter than its 4-byte header length")
	}
	n := uint64(binary.LittleEndian.Uint32(payload))
	rest := payload[4:]
	if n > uint64(len(rest)) {
		return nil, nil, fmt.Errorf("call header length %d exceeds the %d bytes that follow", n, len(rest))
	}
	return rest[:n], rest[n:], nil
}
