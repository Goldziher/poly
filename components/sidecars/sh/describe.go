package main

import (
	"fmt"
	"runtime/debug"
)

const (
	engineName     = "shfmt"
	languageShell  = "shell"
	shellModule    = "mvdan.cc/sh/v3"
	optionsEnabled = "enabled"
	optionsTabs    = "use_tabs"
	// optionTypeBoolean is the boolean bit of WireOptionKey.types.
	optionTypeBoolean = 1
)

type wireCapabilities struct {
	Lint    bool `msgpack:"lint"`
	Format  bool `msgpack:"format"`
	Fix     bool `msgpack:"fix"`
	Project bool `msgpack:"project"`
}

type wireOptionKey struct {
	Name  string `msgpack:"name"`
	Types uint8  `msgpack:"types"`
}

type wireOptionKeys struct {
	Table         string          `msgpack:"table"`
	Checked       bool            `msgpack:"checked"`
	Declared      []wireOptionKey `msgpack:"declared"`
	RuleSelection bool            `msgpack:"rule_selection"`
	HasDerived    bool            `msgpack:"has_derived"`
	Note          *string         `msgpack:"note"`
}

type wireEngineDesc struct {
	Name                       string           `msgpack:"name"`
	Languages                  []string         `msgpack:"languages"`
	Capabilities               wireCapabilities `msgpack:"capabilities"`
	Version                    string           `msgpack:"version"`
	OptionKeys                 []wireOptionKeys `msgpack:"option_keys"`
	Hooks                      uint32           `msgpack:"hooks"`
	TakesOverTierOne           bool             `msgpack:"takes_over_tier_one"`
	ClaimedPaths               []string         `msgpack:"claimed_paths"`
	HostHook                   string           `msgpack:"host_hook"`
	SupplementsTierOne         bool             `msgpack:"supplements_tier_one"`
	SelfManagesEnabled         bool             `msgpack:"self_manages_enabled"`
	SupersedesGenericFormatter bool             `msgpack:"supersedes_generic_formatter"`
	BatchShardFloor            uint32           `msgpack:"batch_shard_floor"`
}

type wireDescribe struct {
	Protocol         uint32           `msgpack:"protocol"`
	ComponentVersion string           `msgpack:"component_version"`
	Engines          []wireEngineDesc `msgpack:"engines"`
}

// shfmtVersion is the version of the mvdan.cc/sh/v3 module this binary was
// built with.
func shfmtVersion() (string, error) {
	info, ok := debug.ReadBuildInfo()
	if !ok {
		return "", fmt.Errorf("no build info in this binary")
	}
	for _, dep := range info.Deps {
		if dep.Path == shellModule {
			return dep.Version, nil
		}
	}
	return "", fmt.Errorf("build info does not list %s", shellModule)
}

// describe is poly's `describe()`: the one engine this component hosts. It
// must equal the component index entry the host resolved the component from.
func describe(componentVersion string) (wireDescribe, error) {
	version, err := shfmtVersion()
	if err != nil {
		return wireDescribe{}, err
	}
	boolean := func(name string) wireOptionKey { return wireOptionKey{Name: name, Types: optionTypeBoolean} }
	return wireDescribe{
		Protocol:         protocolVersion,
		ComponentVersion: componentVersion,
		Engines: []wireEngineDesc{{
			Name:         engineName,
			Languages:    []string{languageShell},
			Capabilities: wireCapabilities{Format: true},
			Version:      version,
			OptionKeys: []wireOptionKeys{
				{Table: "Lint", Checked: true, Declared: []wireOptionKey{boolean(optionsEnabled)}},
				{Table: "Format", Checked: true, Declared: []wireOptionKey{boolean(optionsEnabled), boolean(optionsTabs)}},
				{Table: "CrossCuttingLint", Checked: true, Declared: []wireOptionKey{}},
			},
			ClaimedPaths:    []string{},
			HostHook:        "editorconfig",
			BatchShardFloor: 2,
		}},
	}, nil
}
