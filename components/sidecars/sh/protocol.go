package main

import (
	"bytes"
	"errors"
	"fmt"

	"github.com/vmihailenco/msgpack/v5"
)

// protocolVersion is poly's component protocol version
// (poly_component_protocol::PROTOCOL_VERSION).
const protocolVersion = 2

// Wire error kinds (WireErrorKind).
const (
	errKindError       = "Error"
	errKindUnknownCfg  = "UnknownConfig"
	errKindUnknownEng  = "UnknownEngine"
	errKindProtocol    = "Protocol"
	errKindUnsupported = "Unsupported"
)

// Wire log levels (WireLogLevel).
const logDebug = "Debug"

// wireError mirrors poly_component_protocol::WireError.
type wireError struct {
	Kind    string   `msgpack:"kind"`
	Message string   `msgpack:"message"`
	Chain   []string `msgpack:"chain"`
}

func (e *wireError) Error() string { return e.Kind + ": " + e.Message }

func newWireError(kind, format string, args ...any) *wireError {
	return &wireError{Kind: kind, Message: fmt.Sprintf(format, args...), Chain: []string{}}
}

// wireLog mirrors poly_component_protocol::WireLog.
type wireLog struct {
	Level   string `msgpack:"level"`
	Target  string `msgpack:"target"`
	Message string `msgpack:"message"`
}

// wireFile mirrors poly_component_protocol::WireFile.
type wireFile struct {
	Path     string `msgpack:"path"`
	Language string `msgpack:"language"`
	Len      uint64 `msgpack:"len"`
}

// request is a decoded request header: the variant name and its fields.
type request struct {
	variant string
	fields  msgpack.RawMessage
}

// decodeRequest splits an externally tagged request header
// (`{"Variant": {fields}}`) into its variant and raw fields.
func decodeRequest(header []byte) (request, error) {
	var tagged map[string]msgpack.RawMessage
	if err := msgpack.Unmarshal(header, &tagged); err != nil {
		return request{}, fmt.Errorf("decode request header: %w", err)
	}
	if len(tagged) != 1 {
		return request{}, fmt.Errorf("request header has %d variants, want 1", len(tagged))
	}
	for variant, fields := range tagged {
		return request{variant: variant, fields: fields}, nil
	}
	return request{}, errors.New("unreachable")
}

// initRequest mirrors Request::Init.
type initRequest struct {
	Protocol uint32 `msgpack:"protocol"`
}

// registerConfigRequest mirrors Request::RegisterConfig; Config is a WireValue
// table.
type registerConfigRequest struct {
	ID     uint32 `msgpack:"id"`
	Config any    `msgpack:"config"`
}

// formatRequest mirrors Request::Format.
type formatRequest struct {
	Engine string   `msgpack:"engine"`
	Config uint32   `msgpack:"config"`
	File   wireFile `msgpack:"file"`
}

// engineConfig is the part of a registered engine config this component reads.
type engineConfig struct {
	indentWidth uint
	useTabs     bool
}

// parseEngineConfig reads the `indent_width` and `options.use_tabs` keys of a
// WireValue config table.
func parseEngineConfig(value any) (engineConfig, error) {
	table, err := wireTable(value)
	if err != nil {
		return engineConfig{}, fmt.Errorf("engine config: %w", err)
	}
	var cfg engineConfig
	width, ok := table["indent_width"]
	if !ok {
		return cfg, errors.New("engine config is missing indent_width")
	}
	n, err := wireInteger(width)
	if err != nil || n < 0 {
		return cfg, fmt.Errorf("engine config indent_width: want a non-negative integer, got %v", width)
	}
	cfg.indentWidth = uint(n)
	if options, ok := table["options"]; ok {
		opts, err := wireTable(options)
		if err != nil {
			return cfg, fmt.Errorf("engine config options: %w", err)
		}
		if tabs, ok := opts["use_tabs"]; ok {
			cfg.useTabs, err = wireBoolean(tabs)
			if err != nil {
				return cfg, fmt.Errorf("engine config use_tabs: %w", err)
			}
		}
	}
	return cfg, nil
}

// wireTable unwraps a WireValue::Table (`{"Table": [[key, value], ...]}`) into
// a map of still-wrapped values.
func wireTable(value any) (map[string]any, error) {
	inner, err := wireVariant(value, "Table")
	if err != nil {
		return nil, err
	}
	pairs, ok := inner.([]any)
	if !ok {
		return nil, fmt.Errorf("table body is %T, want an array of pairs", inner)
	}
	table := make(map[string]any, len(pairs))
	for _, pair := range pairs {
		kv, ok := pair.([]any)
		if !ok || len(kv) != 2 {
			return nil, fmt.Errorf("table entry is %v, want a [key, value] pair", pair)
		}
		key, ok := kv[0].(string)
		if !ok {
			return nil, fmt.Errorf("table key is %T, want a string", kv[0])
		}
		table[key] = kv[1]
	}
	return table, nil
}

func wireInteger(value any) (int64, error) {
	inner, err := wireVariant(value, "Integer")
	if err != nil {
		return 0, err
	}
	switch n := inner.(type) {
	case int8:
		return int64(n), nil
	case int16:
		return int64(n), nil
	case int32:
		return int64(n), nil
	case int64:
		return n, nil
	case uint8:
		return int64(n), nil
	case uint16:
		return int64(n), nil
	case uint32:
		return int64(n), nil
	case uint64:
		if n > 1<<63-1 {
			return 0, fmt.Errorf("integer %d overflows int64", n)
		}
		return int64(n), nil
	default:
		return 0, fmt.Errorf("integer body is %T", inner)
	}
}

func wireBoolean(value any) (bool, error) {
	inner, err := wireVariant(value, "Boolean")
	if err != nil {
		return false, err
	}
	b, ok := inner.(bool)
	if !ok {
		return false, fmt.Errorf("boolean body is %T", inner)
	}
	return b, nil
}

// wireVariant unwraps a single-entry externally tagged map and checks its tag.
func wireVariant(value any, tag string) (any, error) {
	m, ok := value.(map[string]any)
	if !ok || len(m) != 1 {
		return nil, fmt.Errorf("want a {%q: ...} value, got %v", tag, value)
	}
	inner, ok := m[tag]
	if !ok {
		return nil, fmt.Errorf("want variant %q, got %v", tag, value)
	}
	return inner, nil
}

// encodeResponse encodes `Response { result, logs }`: `ok` is the payload of a
// success, or errValue the failure.
func encodeResponse(ok any, failure *wireError, logs []wireLog) ([]byte, error) {
	var result map[string]any
	if failure != nil {
		result = map[string]any{"Err": failure}
	} else {
		result = map[string]any{"Ok": ok}
	}
	if logs == nil {
		logs = []wireLog{}
	}
	var buf bytes.Buffer
	enc := msgpack.NewEncoder(&buf)
	enc.SetCustomStructTag("msgpack")
	if err := enc.Encode(map[string]any{"result": result, "logs": logs}); err != nil {
		return nil, fmt.Errorf("encode response: %w", err)
	}
	return buf.Bytes(), nil
}
