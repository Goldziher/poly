package main

import (
	"fmt"
	"unicode/utf8"

	"github.com/vmihailenco/msgpack/v5"
)

// server answers poly's component protocol. It is used from one goroutine:
// the host serializes every call to a sidecar.
type server struct {
	componentVersion string
	initialized      bool
	configs          map[uint32]engineConfig
}

func newServer(componentVersion string) *server {
	return &server{componentVersion: componentVersion, configs: make(map[uint32]engineConfig)}
}

// describe answers the `describe` method.
func (s *server) describe() ([]byte, error) {
	desc, err := describe(s.componentVersion)
	if err != nil {
		return nil, err
	}
	out, err := msgpack.Marshal(desc)
	if err != nil {
		return nil, fmt.Errorf("encode describe: %w", err)
	}
	return out, nil
}

// call answers the `call` method: one request header and the file content.
// A failure the protocol can express is a Response with an error result, not a
// Go error; only an undecodable header is a Go error.
func (s *server) call(header, body []byte) ([]byte, error) {
	req, err := decodeRequest(header)
	if err != nil {
		return nil, err
	}
	var (
		payload any
		logs    []wireLog
		failure *wireError
	)
	switch req.variant {
	case "Init":
		payload, failure = s.handleInit(req.fields)
	case "RegisterConfig":
		payload, failure = s.handleRegisterConfig(req.fields)
	case "Format":
		payload, logs, failure = s.handleFormat(req.fields, body)
	default:
		failure = newWireError(errKindUnsupported, "the sh component does not serve %s", req.variant)
	}
	return encodeResponse(payload, failure, logs)
}

func (s *server) handleInit(fields msgpack.RawMessage) (any, *wireError) {
	var init initRequest
	if err := msgpack.Unmarshal(fields, &init); err != nil {
		return nil, newWireError(errKindProtocol, "malformed Init: %v", err)
	}
	if init.Protocol != protocolVersion {
		return nil, newWireError(errKindProtocol, "host speaks protocol %d, this component speaks %d", init.Protocol, protocolVersion)
	}
	s.initialized = true
	return map[string]any{"Init": map[string]any{"protocol": uint32(protocolVersion), "component_version": s.componentVersion}}, nil
}

func (s *server) handleRegisterConfig(fields msgpack.RawMessage) (any, *wireError) {
	var reg registerConfigRequest
	if err := msgpack.Unmarshal(fields, &reg); err != nil {
		return nil, newWireError(errKindProtocol, "malformed RegisterConfig: %v", err)
	}
	cfg, err := parseEngineConfig(reg.Config)
	if err != nil {
		return nil, newWireError(errKindProtocol, "%v", err)
	}
	s.configs[reg.ID] = cfg
	return "Ok", nil
}

func (s *server) handleFormat(fields msgpack.RawMessage, body []byte) (any, []wireLog, *wireError) {
	var req formatRequest
	if err := msgpack.Unmarshal(fields, &req); err != nil {
		return nil, nil, newWireError(errKindProtocol, "malformed Format: %v", err)
	}
	if req.Engine != engineName {
		return nil, nil, newWireError(errKindUnknownEng, "engine %q is not hosted here", req.Engine)
	}
	cfg, ok := s.configs[req.Config]
	if !ok {
		return nil, nil, newWireError(errKindUnknownCfg, "config %d is not registered", req.Config)
	}
	if req.File.Len != uint64(len(body)) {
		return nil, nil, newWireError(errKindProtocol, "file.len is %d but the body has %d bytes", req.File.Len, len(body))
	}
	if !utf8.Valid(body) {
		return nil, nil, newWireError(errKindError, "%s is not valid UTF-8", req.File.Path)
	}
	indent := cfg.indentWidth
	if cfg.useTabs {
		indent = 0
	}
	formatted, changed, err := formatSource(req.File.Path, body, formatOptions{indent: indent})
	if err != nil {
		// Like the host tool's non-zero exit: a file that does not parse is
		// left alone, with the reason at debug level.
		log := wireLog{Level: logDebug, Target: "poly_sh", Message: err.Error()}
		return map[string]any{"Formatted": "Unchanged"}, []wireLog{log}, nil
	}
	if !changed {
		return map[string]any{"Formatted": "Unchanged"}, nil, nil
	}
	return map[string]any{"Formatted": map[string]any{"Formatted": formatted}}, nil, nil
}
